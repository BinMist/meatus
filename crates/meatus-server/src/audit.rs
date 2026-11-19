#![forbid(unsafe_code)]
//! 审计日志：JSONL 按天落盘，内存保留最近事件，磁盘查询覆盖 90 天。

use std::collections::VecDeque;
use std::fs::{OpenOptions, read_dir};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tracing::warn;

const MEMORY_CAPACITY: usize = 2_000;
const MAX_QUERY_LIMIT: usize = 500;
const MAX_SCAN_LINES: usize = 50_000;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AuditEvent {
    pub ts_ms: i64,
    pub actor: String,
    pub source_ip: String,
    pub action: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    pub result: String,
    pub trace_id: String,
}

impl AuditEvent {
    #[must_use]
    pub fn new(action: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            ts_ms: now_ms(),
            actor: "admin".to_string(),
            source_ip: String::new(),
            action: action.into(),
            target: target.into(),
            client_id: None,
            result: "ok".to_string(),
            trace_id: String::new(),
        }
    }

    #[must_use]
    pub fn actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = actor.into();
        self
    }

    #[must_use]
    pub fn source_ip(mut self, ip: IpAddr) -> Self {
        self.source_ip = ip.to_string();
        self
    }

    #[must_use]
    pub fn client(mut self, client_id: impl Into<String>) -> Self {
        self.client_id = Some(client_id.into());
        self
    }

    #[must_use]
    pub fn result(mut self, result: impl Into<String>) -> Self {
        self.result = result.into();
        self
    }

    #[must_use]
    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = trace_id.into();
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub days: u64,
    pub limit: usize,
    pub actor: Option<String>,
    pub action: Option<String>,
    pub client_id: Option<String>,
    pub result: Option<String>,
}

impl AuditFilter {
    fn normalize(mut self) -> Self {
        self.days = self.days.clamp(1, 90);
        self.limit = self.limit.clamp(1, MAX_QUERY_LIMIT);
        self
    }

    fn matches(&self, event: &AuditEvent, cutoff_ms: i64) -> bool {
        if event.ts_ms < cutoff_ms {
            return false;
        }
        if let Some(actor) = &self.actor
            && !event.actor.eq_ignore_ascii_case(actor)
        {
            return false;
        }
        if let Some(action) = &self.action
            && !event.action.eq_ignore_ascii_case(action)
        {
            return false;
        }
        if let Some(client_id) = &self.client_id
            && event.client_id.as_deref() != Some(client_id.as_str())
            && event.target != *client_id
        {
            return false;
        }
        if let Some(result) = &self.result
            && !event.result.eq_ignore_ascii_case(result)
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    dir: Option<PathBuf>,
    inner: Arc<Mutex<VecDeque<AuditEvent>>>,
    trace_seq: Arc<AtomicU64>,
    last_prune_day: Arc<AtomicU64>,
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new(None)
    }
}

impl AuditLog {
    #[must_use]
    pub fn new(dir: Option<PathBuf>) -> Self {
        if let Some(dir) = &dir
            && let Err(err) = std::fs::create_dir_all(dir)
        {
            warn!(dir = %dir.display(), error = %err, "audit directory unavailable; memory only");
            return Self::memory_only();
        }
        Self {
            dir,
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(256))),
            trace_seq: Arc::new(AtomicU64::new(0)),
            last_prune_day: Arc::new(AtomicU64::new(0)),
        }
    }

    fn memory_only() -> Self {
        Self {
            dir: None,
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(256))),
            trace_seq: Arc::new(AtomicU64::new(0)),
            last_prune_day: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 审计目录；未配置配置文件路径时为 `None`，只保留内存事件。
    #[must_use]
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.dir.is_some()
    }

    pub fn record(&self, mut event: AuditEvent) {
        event.actor = sanitize(&event.actor);
        event.target = sanitize(&event.target);
        event.source_ip = sanitize(&event.source_ip);
        if event.trace_id.is_empty() {
            let seq = self.trace_seq.fetch_add(1, Ordering::Relaxed);
            event.trace_id = format!("{:016x}", (event.ts_ms as u64) ^ seq.rotate_left(17));
        }
        let line = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.len() >= MEMORY_CAPACITY {
                inner.pop_front();
            }
            inner.push_back(event.clone());
        }
        if let Some(dir) = &self.dir {
            let path = dir.join(format!("audit-{}.jsonl", date_key(event.ts_ms)));
            match append_line(&path, &line) {
                Ok(()) => self.prune(dir),
                Err(err) => warn!(
                    path = %path.display(),
                    error = %err,
                    "audit write failed; forwarding continues"
                ),
            }
        }
    }

    #[must_use]
    pub fn query(&self, filter: &AuditFilter) -> Vec<AuditEvent> {
        let filter = filter.clone().normalize();
        let cutoff_ms = now_ms() - (filter.days as i64) * 86_400_000;
        if let Some(dir) = &self.dir {
            match query_disk(dir, &filter, cutoff_ms) {
                Ok(events) => return events,
                Err(err) => {
                    warn!(dir = %dir.display(), error = %err, "audit query fell back to memory")
                }
            }
        }
        query_memory(&self.inner, &filter, cutoff_ms)
    }

    fn prune(&self, dir: &Path) {
        let today = now_ms() / 86_400_000;
        let last = self.last_prune_day.load(Ordering::Relaxed);
        if last == today as u64 {
            return;
        }
        self.last_prune_day.store(today as u64, Ordering::Relaxed);
        let cutoff = date_key(now_ms() - 90 * 86_400_000);
        let Ok(entries) = read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(date) = name
                .strip_prefix("audit-")
                .and_then(|rest| rest.strip_suffix(".jsonl"))
            else {
                continue;
            };
            if date < cutoff.as_str() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
}

fn query_disk(
    dir: &Path,
    filter: &AuditFilter,
    cutoff_ms: i64,
) -> std::io::Result<Vec<AuditEvent>> {
    let cutoff_date = date_key(cutoff_ms);
    let mut files: Vec<PathBuf> = read_dir(dir)?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let date = name
                .strip_prefix("audit-")
                .and_then(|rest| rest.strip_suffix(".jsonl"))?;
            (date >= cutoff_date.as_str()).then(|| entry.path())
        })
        .collect();
    files.sort();
    files.reverse();

    let mut scanned = 0usize;
    let mut matched = Vec::new();
    'files: for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines().rev() {
            scanned += 1;
            if scanned > MAX_SCAN_LINES {
                break 'files;
            }
            let Ok(event) = serde_json::from_str::<AuditEvent>(line) else {
                continue;
            };
            if filter.matches(&event, cutoff_ms) {
                matched.push(event);
                if matched.len() >= filter.limit {
                    break 'files;
                }
            }
        }
    }
    matched.sort_by_key(|event| event.ts_ms);
    matched.reverse();
    matched.truncate(filter.limit);
    Ok(matched)
}

fn query_memory(
    inner: &Arc<Mutex<VecDeque<AuditEvent>>>,
    filter: &AuditFilter,
    cutoff_ms: i64,
) -> Vec<AuditEvent> {
    let inner = inner
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut matched: Vec<AuditEvent> = inner
        .iter()
        .filter(|event| filter.matches(event, cutoff_ms))
        .cloned()
        .collect();
    matched.sort_by_key(|event| event.ts_ms);
    matched.reverse();
    matched.truncate(filter.limit);
    matched
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

fn date_key(ts_ms: i64) -> String {
    let days = ts_ms.div_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// 去除审计字段中的换行与控制字符，并遮蔽 token、Cookie、私钥等敏感值。
fn sanitize(value: &str) -> String {
    let mut text = value.replace(['\r', '\n'], " ");
    if text.to_ascii_lowercase().contains("private key-----") {
        return "[redacted private key]".to_string();
    }
    for key in [
        "token=",
        "secret=",
        "password=",
        "cookie=",
        "authorization=",
        "dns_api_token=",
        "api_token=",
        "cf_api_token=",
    ] {
        text = redact_key(&text, key);
    }
    text
}

fn redact_key(text: &str, key: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find(key) {
        let pos = cursor + rel;
        let value_start = pos + key.len();
        output.push_str(&text[cursor..value_start]);
        output.push_str("***");
        let tail = &text[value_start..];
        let end = tail
            .find(|ch: char| ch.is_whitespace() || ch == '&' || ch == ';' || ch == '"')
            .unwrap_or(tail.len());
        cursor = value_start + end;
    }
    output.push_str(&text[cursor..]);
    output
}

#[must_use]
pub fn value_to_json(event: &AuditEvent) -> Value {
    json!(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "meatus-audit-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn records_jsonl_and_filters() {
        let dir = temp_dir("query");
        let log = AuditLog::new(Some(dir.clone()));
        log.record(
            AuditEvent::new("client.create", "nas")
                .client("nas")
                .source_ip("127.0.0.1".parse().unwrap()),
        );
        log.record(AuditEvent::new("proxy.create", "web").result("denied"));
        let all = log.query(&AuditFilter {
            days: 1,
            limit: 10,
            ..AuditFilter::default()
        });
        assert_eq!(all.len(), 2);
        let denied = log.query(&AuditFilter {
            result: Some("denied".to_string()),
            ..AuditFilter::default()
        });
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].action, "proxy.create");
        let files: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .flatten()
            .collect();
        assert_eq!(files.len(), 1, "one file per UTC day");
        let lines = std::fs::read_to_string(files[0].path()).expect("read");
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn redacts_secret_values() {
        assert_eq!(redact_key("token=abc123 next", "token="), "token=*** next");
        assert_eq!(redact_key("x=1&secret=abc", "secret="), "x=1&secret=***");
        assert_eq!(
            redact_key("Admin_Token=abc next", "token="),
            "Admin_Token=*** next"
        );
        assert_eq!(sanitize("line\nbreak"), "line break");
        assert_eq!(
            sanitize("-----BEGIN PRIVATE KEY-----abc"),
            "[redacted private key]"
        );
        assert_eq!(sanitize("Cookie=meatus_session=abc"), "Cookie=***");
    }

    #[test]
    fn date_key_matches_known_day() {
        assert_eq!(date_key(1_797_000_000_000), "2026-12-11");
    }
}

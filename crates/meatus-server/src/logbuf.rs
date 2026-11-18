#![forbid(unsafe_code)]
//! 进程内日志环形缓冲：给 Web 面板提供最近日志，不额外落盘。

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

const DEFAULT_CAPACITY: usize = 2_000;

#[derive(Debug, Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<String>>>,
    capacity: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl LogBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.min(256)))),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.len() >= self.capacity {
            inner.pop_front();
        }
        inner.push_back(line);
    }

    #[must_use]
    pub fn snapshot(&self, limit: usize) -> Vec<String> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let start = inner.len().saturating_sub(limit);
        inner.iter().skip(start).cloned().collect()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// tracing 的 `MakeWriter`：每个事件由这里收口成一行，写入环形缓冲。
pub struct LogWriter {
    buffer: LogBuffer,
    line: String,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogWriter {
            buffer: self.clone(),
            line: String::with_capacity(256),
        }
    }
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.line.push_str(&String::from_utf8_lossy(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        if !self.line.is_empty() {
            self.buffer.push(std::mem::take(&mut self.line));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_latest_lines() {
        let buffer = LogBuffer::new(2);
        buffer.push("a".into());
        buffer.push("b".into());
        buffer.push("c".into());
        assert_eq!(buffer.snapshot(10), vec!["b", "c"]);
        assert_eq!(buffer.snapshot(1), vec!["c"]);
    }

    #[test]
    fn writer_flushes_one_event() {
        let buffer = LogBuffer::new(4);
        {
            use tracing_subscriber::fmt::MakeWriter;
            let mut writer = buffer.make_writer();
            writer.write_all(b"hello ").unwrap();
            writer.write_all(b"world").unwrap();
        }
        assert_eq!(buffer.snapshot(4), vec!["hello world"]);
    }
}

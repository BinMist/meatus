#![forbid(unsafe_code)]
//! 控制面管理命令总线：admin HTTP 线程把增删改命令投递给对应 client 的控制会话。

use std::collections::HashMap;
use std::sync::RwLock;

use meatus_proto::v1::{NewProxy, ProbeResult, ProxyOk};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug)]
pub(crate) enum AdminProxyCommand {
    /// 新增或替换一条运行中的 proxy。`replace` 为 true 时要求同名 proxy 已存在。
    Set {
        request: NewProxy,
        replace: bool,
        reply: oneshot::Sender<Result<ProxyOk, String>>,
    },
    Remove {
        name: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Probe {
        proxy_name: String,
        timeout_ms: u32,
        reply: oneshot::Sender<Result<ProbeResult, String>>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct AdminBus {
    routes: RwLock<HashMap<String, mpsc::Sender<AdminProxyCommand>>>,
}

impl AdminBus {
    pub(crate) fn register(&self, run_id: &str, sender: mpsc::Sender<AdminProxyCommand>) {
        self.routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(run_id.to_string(), sender);
    }

    pub(crate) fn unregister(&self, run_id: &str) {
        self.routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(run_id);
    }

    pub(crate) async fn send(
        &self,
        run_id: &str,
        command: AdminProxyCommand,
    ) -> Result<(), String> {
        let Some(sender) = self
            .routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(run_id)
            .cloned()
        else {
            return Err(format!("client session {run_id} is not connected"));
        };
        sender
            .send(command)
            .await
            .map_err(|_| format!("client session {run_id} command channel closed"))
    }
}

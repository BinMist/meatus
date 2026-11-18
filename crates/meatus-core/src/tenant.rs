#![forbid(unsafe_code)]
//! 租户命名空间约定。
//!
//! v0.x 只有一个租户，但所有资源键从一开始就带 `tenant`，避免未来多租户扩展时返工。

/// v0.x 的固定租户标识。
pub const DEFAULT_TENANT: &str = "default";

/// 生成全局唯一的资源键：`tenant/kind/name`。
#[must_use]
pub fn scoped_key(tenant_id: &str, kind: &str, name: &str) -> String {
    format!("{tenant_id}/{kind}/{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_key_is_tenant_first() {
        assert_eq!(scoped_key("acme", "proxy", "nas-web"), "acme/proxy/nas-web");
        assert_ne!(
            scoped_key("t1", "proxy", "web"),
            scoped_key("t2", "proxy", "web")
        );
    }
}

//! PackageManager 端口的数据类型（主计划 §3.3）。完整端口由 T-O1 落地；
//! 来源结论只依赖这里的 PmKind，因此本任务先把冻结的 PmKind 放到位。

/// 可用的包管理器种类（主计划 §3.3 冻结）。
///
/// 同时派生 Deserialize：§3.3.1 冻结的 OriginVerdict 带 Deserialize，而
/// `PackageManager(PmKind)` 是它的载荷。T-O1 补全本文件时必须保留这两个 derive。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmKind {
    Npm,
    Pnpm,
    Bun,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_kind_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&PmKind::Npm).unwrap(), "\"npm\"");
        assert_eq!(serde_json::to_string(&PmKind::Pnpm).unwrap(), "\"pnpm\"");
        assert_eq!(serde_json::to_string(&PmKind::Bun).unwrap(), "\"bun\"");
    }
}

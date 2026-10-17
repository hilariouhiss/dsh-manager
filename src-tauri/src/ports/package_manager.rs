//! PackageManager 端口（主计划 §3.3，冻结）。
//!
//! 本文件只放 trait 与数据类型；应用层只依赖这里的类型，不 use adapters::*。

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Serialize;

use crate::error::CmdResult;

/// 本工具管理的 DSH 包名（§3.8 D-1：版本源只有这一个包）。
pub const DSH_PACKAGE_NAME: &str = "@deepseek-ai/dsh";

/// 可用的包管理器种类（主计划 §3.3 冻结）。
///
/// 同时派生 Deserialize：§3.3.1 冻结的 OriginVerdict 带 Deserialize，而
/// `PackageManager(PmKind)` 是它的载荷。补全本文件时不得移除这两个 derive。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PmKind {
    Npm,
    Pnpm,
    Bun,
}

impl PmKind {
    pub fn label_zh(self) -> &'static str {
        match self {
            PmKind::Npm => "npm",
            PmKind::Pnpm => "pnpm",
            PmKind::Bun => "bun",
        }
    }
}

/// 一个可用的包管理器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PmInfo {
    pub kind: PmKind,
    pub exe: PathBuf,
    pub version: String,
    pub global_root: PathBuf,
}

/// 一次全局安装的结果。失败**不**用本结构表达，而是以合并输出为 raw 的 Err(AppError)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub exit_code: i32,
    pub output: String,
}

#[async_trait]
pub trait PackageManager: Send + Sync {
    /// 探测本机可用的包管理器；按 npm → pnpm → bun 顺序返回，空表示都不可用。
    async fn available(&self) -> Vec<PmInfo>;
    /// 该包管理器全局安装的 `@deepseek-ai/dsh` 版本；未安装返回 None。
    async fn installed_version(&self, pm: &PmInfo) -> CmdResult<Option<semver::Version>>;
    /// 从本地 tarball 全局安装。**调用方必须已完成 §3.8 的摘要校验。**
    async fn install_from_tarball(&self, pm: &PmInfo, tarball: &Path) -> CmdResult<InstallOutcome>;
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

    #[test]
    fn pm_info_serializes_camel_case() {
        let info = PmInfo {
            kind: PmKind::Npm,
            exe: PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\npm.cmd"),
            version: "11.6.2".to_string(),
            global_root: PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\node_modules"),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["kind"], "npm");
        assert_eq!(json["version"], "11.6.2");
        assert!(json.get("globalRoot").is_some(), "字段名必须是 globalRoot");
        assert!(
            json.get("global_root").is_none(),
            "不得出现 snake_case 字段名"
        );
    }

    #[test]
    fn install_outcome_serializes_camel_case() {
        let json = serde_json::to_value(InstallOutcome {
            exit_code: 0,
            output: "added 1 package".into(),
        })
        .unwrap();
        assert_eq!(json["exitCode"], 0);
        assert_eq!(json["output"], "added 1 package");
    }
}

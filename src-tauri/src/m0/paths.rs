//! 三个数据目录的解析（主计划 §3.6 落盘位置）。
//!
//! 生产路径来自 Tauri 的应用路径接口，**不硬编码 %APPDATA%**；测试用 under_root 注入，
//! 因此单元测试完全不触碰真实用户目录。

use std::fs;
use std::path::{Path, PathBuf};

use tauri::Manager;

use crate::error::{self, AppError, CmdResult};

/// 应用数据 / 本地缓存 / 临时文件三类目录（主计划 §3.3.2 冻结了本结构的方法形状）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPaths {
    /// %APPDATA%\dsh-manager —— 设置、操作记录、实例日志
    pub app_data_dir: PathBuf,
    /// %LOCALAPPDATA%\dsh-manager\cache —— 通道状态缓存
    pub cache_dir: PathBuf,
    /// %LOCALAPPDATA%\Temp\dsh-manager —— 已获取的安装包
    pub temp_dir: PathBuf,
}

impl DataPaths {
    /// 生产路径：三个根目录都取自 Tauri 的应用路径接口（签名见主计划 §3.3.2）。
    pub fn resolve(app: &tauri::AppHandle) -> CmdResult<Self> {
        let resolver = app.path();
        let roaming = resolver
            .config_dir()
            .map_err(|e| path_error("解析应用数据目录", e.to_string()))?;
        let local = resolver
            .local_data_dir()
            .map_err(|e| path_error("解析本地缓存目录", e.to_string()))?;
        let temp = resolver
            .temp_dir()
            .map_err(|e| path_error("解析临时目录", e.to_string()))?;
        Ok(Self {
            app_data_dir: roaming.join("dsh-manager"),
            cache_dir: local.join("dsh-manager").join("cache"),
            temp_dir: temp.join("dsh-manager"),
        })
    }

    /// 可注入路径：把三个目录挂在给定根下（测试与将来的重置功能都用它）。
    pub fn under_root(root: &Path) -> Self {
        Self {
            app_data_dir: root.join("appdata"),
            cache_dir: root.join("localcache"),
            temp_dir: root.join("temp"),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.app_data_dir.join("settings.json")
    }

    pub fn oplog_file(&self) -> PathBuf {
        self.app_data_dir.join("oplog.jsonl")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.app_data_dir.join("logs")
    }

    pub fn instance_log(&self, instance_id: &str) -> PathBuf {
        self.log_dir().join(format!("instance-{instance_id}.log"))
    }

    pub fn cache_file(&self) -> PathBuf {
        self.cache_dir.join("channels.json")
    }

    /// 临时安装包路径（§3.6：%LOCALAPPDATA%\Temp\dsh-manager\<pkg>-<version>.tgz）。
    /// 作用域包名里的路径分隔符替换成 -，否则会意外变成子目录。
    pub fn tarball(&self, package: &str, version: &str) -> PathBuf {
        let safe: String = package
            .chars()
            .map(|c| if c == '/' || c == '\\' { '-' } else { c })
            .collect();
        self.temp_dir.join(format!("{safe}-{version}.tgz"))
    }

    /// 建齐所有目录（幂等）。
    pub fn ensure_dirs(&self) -> CmdResult<()> {
        let logs = self.log_dir();
        for dir in [&self.app_data_dir, &logs, &self.cache_dir, &self.temp_dir] {
            fs::create_dir_all(dir)
                .map_err(|e| path_error(&format!("创建目录 {}", dir.display()), e.to_string()))?;
        }
        Ok(())
    }
}

fn path_error(what: &str, raw: String) -> AppError {
    AppError::new(
        error::PATH_PERMISSION_DENIED,
        format!("{what}失败：目录不可用或没有访问权限"),
        "确认当前用户对 %APPDATA%、%LOCALAPPDATA% 与临时目录有读写权限，然后重试。",
    )
    .with_raw(raw)
}

/// 测试用临时根目录：每个用例一个独立目录，绝不触碰真实的 %APPDATA%。
#[cfg(test)]
pub(crate) fn temp_root(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dsh-manager-test-{}-{}-{}",
        std::process::id(),
        tag,
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_root_layout_matches_contract() {
        let paths = DataPaths::under_root(&temp_root("paths-layout"));
        assert_eq!(
            paths.settings_file(),
            paths.app_data_dir.join("settings.json")
        );
        assert_eq!(paths.oplog_file(), paths.app_data_dir.join("oplog.jsonl"));
        assert_eq!(paths.log_dir(), paths.app_data_dir.join("logs"));
        assert_eq!(
            paths.instance_log("abc"),
            paths.app_data_dir.join("logs").join("instance-abc.log")
        );
        assert_eq!(paths.cache_file(), paths.cache_dir.join("channels.json"));
        assert_eq!(
            paths
                .tarball("@deepseek-ai/dsh", "0.2.0")
                .file_name()
                .unwrap(),
            "@deepseek-ai-dsh-0.2.0.tgz"
        );
    }

    #[test]
    fn ensure_dirs_creates_every_directory_and_is_idempotent() {
        let paths = DataPaths::under_root(&temp_root("paths-ensure"));
        paths.ensure_dirs().unwrap();
        assert!(paths.app_data_dir.is_dir());
        assert!(paths.app_data_dir.join("logs").is_dir());
        assert!(paths.cache_dir.is_dir());
        assert!(paths.temp_dir.is_dir());
        paths.ensure_dirs().unwrap();
    }

    #[test]
    fn test_paths_never_escape_the_injected_root() {
        let root = temp_root("paths-isolated");
        let paths = DataPaths::under_root(&root);
        assert!(paths.app_data_dir.starts_with(&root));
        assert!(paths.cache_dir.starts_with(&root));
        assert!(paths.temp_dir.starts_with(&root));
        assert!(paths.settings_file().starts_with(&root));
        // 真实 %APPDATA% 存在时，测试路径必须与它无关（测试绝不触碰用户目录）
        if let Ok(real_appdata) = std::env::var("APPDATA") {
            assert!(
                !paths.settings_file().starts_with(&real_appdata),
                "测试路径不得落在真实 %APPDATA% 下"
            );
        }
    }
}

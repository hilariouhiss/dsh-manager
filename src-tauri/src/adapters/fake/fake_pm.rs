//! 假包管理器：记录每一次调用，供「识别路径零写操作」这类断言使用。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use semver::Version;

use crate::error::{AppError, CmdResult, INSTALL_FAILED, PATH_PERMISSION_DENIED};
use crate::ports::package_manager::{InstallOutcome, PackageManager, PmInfo, PmKind};

#[derive(Default)]
pub struct FakePm {
    /// available() 返回的清单。
    pub infos: Vec<PmInfo>,
    /// 每个 kind 的「已安装版本」；缺失即 None（未安装）。
    pub installed: Vec<(PmKind, Option<Version>)>,
    /// 这些 kind 的 installed_version 返回错误，用于「单项来源失败」用例。
    pub failures: Vec<PmKind>,
    /// 每个方法的人为延迟，用于超时用例。
    pub delay: Option<Duration>,
    calls: Mutex<Vec<String>>,
}

impl FakePm {
    pub fn new(infos: Vec<PmInfo>) -> Self {
        Self {
            infos,
            ..Default::default()
        }
    }

    pub fn with_installed(mut self, kind: PmKind, version: Option<Version>) -> Self {
        self.installed.push((kind, version));
        self
    }

    pub fn with_failure(mut self, kind: PmKind) -> Self {
        self.failures.push(kind);
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// 所有写路径调用。**必须为空**才满足 FR-2.4。
    pub fn write_calls(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("write:"))
            .collect()
    }

    fn record(&self, entry: String) {
        self.calls.lock().unwrap().push(entry);
    }
}

#[async_trait]
impl PackageManager for FakePm {
    async fn available(&self) -> Vec<PmInfo> {
        self.record("available".to_string());
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        self.infos.clone()
    }

    async fn installed_version(&self, pm: &PmInfo) -> CmdResult<Option<Version>> {
        self.record(format!("installed_version:{}", pm.kind.label_zh()));
        if self.failures.contains(&pm.kind) {
            return Err(AppError::new(
                PATH_PERMISSION_DENIED,
                "假实现注入：读取该包管理器全局目录失败",
                "这是一条测试用错误，不需要处理。",
            ));
        }
        Ok(self
            .installed
            .iter()
            .find(|(k, _)| *k == pm.kind)
            .and_then(|(_, v)| v.clone()))
    }

    async fn install_from_tarball(
        &self,
        pm: &PmInfo,
        _tarball: &Path,
    ) -> CmdResult<InstallOutcome> {
        self.record(format!("write:install_from_tarball:{}", pm.kind.label_zh()));
        Err(AppError::new(
            INSTALL_FAILED,
            "假实现不执行安装",
            "这是一条测试用错误，不需要处理。",
        ))
    }
}

/// 造一个指向给定全局目录的 PmInfo。
pub fn fake_pm_info(kind: PmKind, global_root: PathBuf) -> PmInfo {
    PmInfo {
        kind,
        exe: global_root.join(format!("{}.cmd", kind.label_zh())),
        version: "11.6.2".to_string(),
        global_root,
    }
}

//! npm 适配器。

use std::path::Path;

use async_trait::async_trait;
use semver::Version;

use crate::error::CmdResult;
use crate::ports::package_manager::{InstallOutcome, PackageManager, PmInfo, PmKind};

pub struct NpmAdapter;

#[async_trait]
impl PackageManager for NpmAdapter {
    async fn available(&self) -> Vec<PmInfo> {
        match super::probe(PmKind::Npm).await {
            Some(info) => vec![info],
            None => Vec::new(),
        }
    }

    async fn installed_version(&self, pm: &PmInfo) -> CmdResult<Option<Version>> {
        super::read_installed_version(&pm.global_root)
    }

    async fn install_from_tarball(&self, pm: &PmInfo, tarball: &Path) -> CmdResult<InstallOutcome> {
        super::run_install(pm, &["install", "-g", "--no-audit", "--no-fund"], tarball).await
    }
}

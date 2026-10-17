//! pnpm 适配器。
//!
//! pnpm 的全局布局与 npm 不同：`pnpm root -g` 返回的是全局**工程**根
//! （如 `%LOCALAPPDATA%\pnpm\global\v11`），包实际位于
//! `<root>/<group>/node_modules/@deepseek-ai/dsh`，而不是 `<root>/@deepseek-ai/dsh`。
//! 因此 PmInfo.global_root 保持 `root -g` 原值；读取已装版本必须扫描子目录，
//! T-O2 不得假设 global_root 下直接有 @scope 目录。

use std::path::Path;

use async_trait::async_trait;
use semver::Version;

use crate::error::{AppError, CmdResult, PATH_PERMISSION_DENIED};
use crate::ports::package_manager::{
    DSH_PACKAGE_NAME, InstallOutcome, PackageManager, PmInfo, PmKind,
};

pub struct PnpmAdapter;

fn unreadable_root(global_root: &Path, e: &std::io::Error) -> AppError {
    AppError::new(
        PATH_PERMISSION_DENIED,
        format!("无法读取 pnpm 全局目录：{}（{e}）", global_root.display()),
        "确认该路径对当前用户可读（可能需要以同一用户身份运行本工具），然后重试。",
    )
    .with_raw(e.to_string())
}

/// 在 pnpm 全局布局中查找已安装 DSH：先看 `<root>/node_modules/...`，
/// 再扫 `<root>/*/node_modules/...`；命中多个时取最高 semver。
fn installed_version_at(global_root: &Path) -> CmdResult<Option<Version>> {
    let manifest_in = |base: &Path| {
        base.join("node_modules")
            .join(DSH_PACKAGE_NAME)
            .join("package.json")
    };
    let mut manifests = vec![manifest_in(global_root)];
    match std::fs::read_dir(global_root) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| unreadable_root(global_root, &e))?;
                manifests.push(manifest_in(&entry.path()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(unreadable_root(global_root, &e)),
    }
    let mut best: Option<Version> = None;
    for manifest in &manifests {
        if let Some(version) = super::read_installed_version_at(manifest)? {
            best = Some(match best {
                Some(current) => current.max(version),
                None => version,
            });
        }
    }
    Ok(best)
}

#[async_trait]
impl PackageManager for PnpmAdapter {
    async fn available(&self) -> Vec<PmInfo> {
        match super::probe(PmKind::Pnpm).await {
            Some(info) => vec![info],
            None => Vec::new(),
        }
    }

    async fn installed_version(&self, pm: &PmInfo) -> CmdResult<Option<Version>> {
        installed_version_at(&pm.global_root)
    }

    async fn install_from_tarball(&self, pm: &PmInfo, tarball: &Path) -> CmdResult<InstallOutcome> {
        super::run_install(pm, &["add", "-g"], tarball).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// 每个用例一个独立临时目录，避免并行测试互相踩。
    fn temp_dir(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("dsh-manager-pnpm-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 在给定基目录下造出 `node_modules/@deepseek-ai/dsh/package.json`。
    fn write_manifest(base: &Path, version: &str) {
        let pkg = base.join("node_modules").join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
    }

    /// 真实的 pnpm 布局：包藏在 `<root>/<group>/node_modules` 下。
    #[test]
    fn picks_the_highest_version_from_nested_group_dirs() {
        let root = temp_dir("nested");
        write_manifest(&root.join("1734-aaaa"), "0.2.0");
        write_manifest(&root.join("9999-bbbb"), "0.3.0");
        assert_eq!(
            installed_version_at(&root).unwrap(),
            Some(Version::parse("0.3.0").unwrap())
        );
    }

    #[test]
    fn reads_the_direct_node_modules_layout() {
        let root = temp_dir("direct");
        write_manifest(&root, "0.2.1-alpha.4");
        assert_eq!(
            installed_version_at(&root).unwrap(),
            Some(Version::parse("0.2.1-alpha.4").unwrap())
        );
    }

    #[test]
    fn returns_none_when_no_dsh_is_installed() {
        let root = temp_dir("absent");
        assert_eq!(installed_version_at(&root).unwrap(), None);
        assert_eq!(
            installed_version_at(&root.join("missing")).unwrap(),
            None,
            "全局根不存在同样必须是 Ok(None)"
        );
    }
}

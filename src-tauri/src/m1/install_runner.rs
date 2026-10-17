//! 安装执行：安装前运行检测 → 安装 → 安装后复核（主计划 T-V6）。
//!
//! **绝不强制结束任何进程**（FR-1.6）。运行检测只问「有没有 Web 形态实例在跑」，
//! 具体怎么枚举进程是 M3 的实现细节，本模块只依赖这个最小端口。

use std::path::Path;

use crate::error::{AppError, CmdResult};
use crate::m1::channels::PACKAGE_NAME;
use crate::ports::package_manager::{PackageManager, PmInfo, PmKind};

// §3.3.1 冻结在 m3/instance_discovery.rs：本模块**只 use，不定义**。
// 「有没有实例在跑」由调用方用 `list_web_instances()?.is_empty()` 判断（M1 不自己数进程）。
use crate::m3::instance_discovery::{InstanceInfo, InstanceLister};

/// 一次安装的完整结果。
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub pm_kind: PmKind,
    pub installed_before: Option<String>,
    pub installed_after: Option<String>,
    pub exit_code: i32,
    pub output: String,
    /// 临时安装包是否保留在临时目录（失败时必须保留，供手工安装）
    pub tarball_kept: bool,
}

/// 手工安装命令。失败时的 `next_step` 用它，让用户能自己把安装做完（FR-1.6）。
///
/// 注意：**只有校验通过但安装失败**才走这条路径（§3.6 消歧）；
/// 校验不通过的文件已在 T-V5 被删除，不会有手工安装的机会。
pub fn manual_command(pm: &PmInfo, tarball: &Path) -> String {
    let installer = match pm.kind {
        PmKind::Npm => "npm",
        PmKind::Pnpm => "pnpm",
        PmKind::Bun => "bun",
    };
    format!("{} install -g \"{}\"", installer, tarball.display())
}

fn instances_running_error(running: &[InstanceInfo]) -> AppError {
    AppError::new(
        crate::error::INSTALL_INSTANCES_RUNNING,
        format!(
            "检测到 {} 个 DSH Web 实例正在运行（pid：{}），安装前需要先关闭。",
            running.len(),
            running
                .iter()
                .map(|i| i.pid.to_string())
                .collect::<Vec<_>>()
                .join("、")
        ),
        "到「运行实例」页停止本工具启动的实例；若是你自己启动的实例或桌面端，请自行退出后重试。本工具不会强制结束任何进程。",
    )
    .retryable(true)
}

/// 执行安装。顺序：**先枚举实例**（有则拒绝）→ 安装 → 重新读取本机版本。
pub async fn run(
    tarball: &Path,
    pms: &[PmInfo],
    pm: &dyn PackageManager,
    lister: &dyn InstanceLister,
) -> CmdResult<InstallReport> {
    // 安装前：枚举 Web 形态实例（调 M3 的能力，不自行实现；§3.3.1 的方法是同步的）
    let running = lister.list_web_instances()?;
    if !running.is_empty() {
        return Err(instances_running_error(&running));
    }
    let Some(chosen) = pms.first() else {
        return Err(AppError::new(
            crate::error::INSTALL_PM_MISSING,
            "本机没有找到可用的包管理器（npm / pnpm / bun 都没有探测到）。",
            "安装 Node.js（自带 npm）后重启本应用，再到「安装来源」页确认能被识别。",
        ));
    };
    let installed_before = pm.installed_version(chosen).await?.map(|v| v.to_string());

    let outcome = match pm.install_from_tarball(chosen, tarball).await {
        Ok(o) => o,
        Err(e) => {
            // 失败时**保留已获取的安装包**，并把手工命令写进下一步（FR-1.6）
            let command = manual_command(chosen, tarball);
            return Err(AppError::new(
                crate::error::INSTALL_FAILED,
                format!(
                    "安装失败：{} 返回非零退出码。安装包已保留在 {}。",
                    chosen.exe.display(),
                    tarball.display()
                ),
                format!("在终端里手工执行：{command}"),
            )
            .with_raw(e.raw.unwrap_or_else(|| e.reason.clone()))
            .retryable(true));
        }
    };
    if outcome.exit_code != 0 {
        let command = manual_command(chosen, tarball);
        return Err(AppError::new(
            crate::error::INSTALL_FAILED,
            format!(
                "安装失败：{} 退出码 {}。安装包已保留在 {}。",
                chosen.exe.display(),
                outcome.exit_code,
                tarball.display()
            ),
            format!("在终端里手工执行：{command}"),
        )
        .with_raw(outcome.output.clone())
        .retryable(true));
    }

    let installed_after = pm.installed_version(chosen).await?.map(|v| v.to_string());
    Ok(InstallReport {
        pm_kind: chosen.kind,
        installed_before,
        installed_after,
        exit_code: outcome.exit_code,
        output: outcome.output,
        tarball_kept: false,
    })
}

/// 成功安装后清理临时包。**失败路径绝不调用它**（FR-1.6 要求保留安装包）。
pub fn cleanup_after_success(report: &InstallReport, tarball: &Path) -> CmdResult<()> {
    if report.tarball_kept {
        return Ok(());
    }
    if tarball.exists() {
        std::fs::remove_file(tarball).map_err(|e| {
            AppError::new(
                crate::error::PATH_PERMISSION_DENIED,
                format!("安装已完成，但清理临时包 {} 失败：{e}", tarball.display()),
                "可以手动删除该文件，不影响已完成的安装。",
            )
        })?;
    }
    Ok(())
}

/// 临时包路径：**复用 §3.6 冻结的 `DataPaths::tarball()`**，不自己拼。
///
/// 作用域包名 `@deepseek-ai/dsh` 里的 `/` 会被净化成 `-`，得到
/// `@deepseek-ai-dsh-<version>.tgz`；自己拼会产生子目录（审计重要-9）。
pub fn tarball_path(
    paths: &crate::m0::paths::DataPaths,
    version: &semver::Version,
) -> std::path::PathBuf {
    paths.tarball(PACKAGE_NAME, &version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m3::InstanceState;
    use crate::m3::instance_discovery::{InstanceInfo, InstanceLister};
    use crate::ports::package_manager::{InstallOutcome, PmInfo, PmKind};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假实例枚举：实现 §3.3.1 冻结的 `InstanceLister`（**同步**方法，名字固定）。
    struct FakeLister {
        running: Vec<InstanceInfo>,
        calls: AtomicUsize,
    }

    impl FakeLister {
        fn with_running(pids: &[u32]) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                running: pids
                    .iter()
                    .map(|pid| InstanceInfo {
                        instance_id: format!("fake-{pid}"),
                        managed: false,
                        pid: *pid,
                        started_at_ms: Some(1_760_000_000_000),
                        uptime_ms: Some(1_000),
                        url: Some("http://127.0.0.1:3080".to_string()),
                        state: InstanceState::Running,
                    })
                    .collect(),
            }
        }

        fn empty() -> Self {
            Self::with_running(&[])
        }
    }

    impl InstanceLister for FakeLister {
        fn list_web_instances(&self) -> crate::error::CmdResult<Vec<InstanceInfo>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.running.clone())
        }
    }

    struct RecordingPm {
        outcomes: Mutex<Vec<Result<InstallOutcome, (String, String)>>>,
        versions: Mutex<Vec<Option<String>>>,
        installed: Mutex<Vec<PathBuf>>,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingPm {
        fn new(
            outcomes: Vec<Result<InstallOutcome, (String, String)>>,
            versions: Vec<Option<&str>>,
        ) -> Self {
            Self {
                outcomes: Mutex::new(outcomes),
                versions: Mutex::new(
                    versions
                        .into_iter()
                        .map(|v| v.map(str::to_string))
                        .collect(),
                ),
                installed: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl PackageManager for RecordingPm {
        async fn available(&self) -> Vec<PmInfo> {
            vec![]
        }

        async fn installed_version(
            &self,
            _pm: &PmInfo,
        ) -> crate::error::CmdResult<Option<semver::Version>> {
            self.calls
                .lock()
                .unwrap()
                .push("installed_version".to_string());
            let mut versions = self.versions.lock().unwrap();
            let next = if versions.len() > 1 {
                versions.remove(0)
            } else {
                versions.first().cloned().flatten()
            };
            Ok(next.map(|v| semver::Version::parse(&v).unwrap()))
        }

        async fn install_from_tarball(
            &self,
            _pm: &PmInfo,
            tarball: &Path,
        ) -> crate::error::CmdResult<InstallOutcome> {
            self.calls
                .lock()
                .unwrap()
                .push("install_from_tarball".to_string());
            self.installed.lock().unwrap().push(tarball.to_path_buf());
            match self.outcomes.lock().unwrap().remove(0) {
                Ok(o) => Ok(o),
                Err((code, output)) => Err(crate::error::AppError::new(
                    code.as_str(),
                    "安装子进程非零退出。",
                    "按提示手工安装。",
                )
                .with_raw(output)),
            }
        }
    }

    fn pm_info() -> PmInfo {
        PmInfo {
            kind: PmKind::Npm,
            exe: PathBuf::from("C:\\Program Files\\nodejs\\npm.cmd"),
            version: "11.6.0".to_string(),
            global_root: PathBuf::from("C:\\Users\\u\\AppData\\Roaming\\npm"),
        }
    }

    fn tmp_tarball(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dsh-manager-runner-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 文件名与 §3.6 的净化规则一致：@deepseek-ai-dsh-<version>.tgz
        let p = dir.join("@deepseek-ai-dsh-0.2.0-rc.2.tgz");
        std::fs::write(&p, b"pretend tarball").unwrap();
        p
    }

    fn pm_only() -> Vec<PmInfo> {
        vec![pm_info()]
    }

    #[test]
    fn manual_command_is_runnable_with_a_space_before_the_quoted_path() {
        assert_eq!(
            manual_command(
                &pm_info(),
                Path::new("C:\\Temp\\@deepseek-ai-dsh-0.2.0.tgz")
            ),
            "npm install -g \"C:\\Temp\\@deepseek-ai-dsh-0.2.0.tgz\"",
        );
    }

    #[tokio::test]
    async fn running_instances_refuse_the_install_without_killing_anything() {
        let tarball = tmp_tarball("instances");
        let lister = FakeLister::with_running(&[4242]);
        let pm = RecordingPm::new(vec![], vec![Some("0.2.0-rc.2")]);
        let e = run(&tarball, &pm_only(), &pm, &lister).await.unwrap_err();
        assert_eq!(e.code, crate::error::INSTALL_INSTANCES_RUNNING);
        assert!(e.retryable, "用户关掉实例后可以重试");
        assert!(
            e.reason.contains("正在运行"),
            "原因要说明有实例在跑：{}",
            e.reason
        );
        assert!(
            e.next_step.contains("停止") || e.next_step.contains("退出"),
            "下一步要告诉用户怎么处理：{}",
            e.next_step
        );
        assert!(
            pm.installed.lock().unwrap().is_empty(),
            "有实例运行时绝不允许走到安装动作"
        );
        assert!(
            pm.calls().is_empty(),
            "有实例运行时包管理器一个方法都不该被调用：{:?}",
            pm.calls()
        );
        assert_eq!(lister.calls.load(Ordering::SeqCst), 1, "安装前必须枚举一次");
        assert!(tarball.exists(), "被拒时不得删除已获取的安装包");
    }

    #[tokio::test]
    async fn successful_install_reports_the_version_read_back_afterwards() {
        let tarball = tmp_tarball("ok");
        let lister = FakeLister::empty();
        let pm = RecordingPm::new(
            vec![Ok(InstallOutcome {
                exit_code: 0,
                output: "added 1 package".to_string(),
            })],
            vec![Some("0.2.0-rc.2"), Some("0.2.1-alpha.4")],
        );
        let report = run(&tarball, &pm_only(), &pm, &lister).await.unwrap();
        assert_eq!(report.installed_before.as_deref(), Some("0.2.0-rc.2"));
        assert_eq!(
            report.installed_after.as_deref(),
            Some("0.2.1-alpha.4"),
            "安装完成后必须重新读取本机版本并如实反馈（FR-1.6）"
        );
        assert_eq!(report.pm_kind, PmKind::Npm);
        assert!(!report.tarball_kept, "成功时临时包可以清理");
    }

    #[tokio::test]
    async fn failed_install_keeps_the_tarball_and_offers_a_manual_command() {
        let tarball = tmp_tarball("fail");
        let lister = FakeLister::empty();
        let pm = RecordingPm::new(
            vec![Err((
                crate::error::INSTALL_FAILED.to_string(),
                "npm ERR! code EACCES\nnpm ERR! permission denied".to_string(),
            ))],
            vec![Some("0.2.0-rc.2"), Some("0.2.0-rc.2")],
        );
        let e = run(&tarball, &pm_only(), &pm, &lister).await.unwrap_err();
        assert_eq!(e.code, crate::error::INSTALL_FAILED);
        assert!(
            e.raw.as_deref().unwrap_or_default().contains("EACCES"),
            "raw 必须是安装子进程的输出（FR-1.6）"
        );
        assert!(
            e.next_step.contains("npm install -g"),
            "下一步必须给出可执行的手工安装命令：{}",
            e.next_step
        );
        assert!(
            e.next_step.contains(tarball.to_string_lossy().as_ref()),
            "手工命令里必须带上这个安装包的完整路径：{}",
            e.next_step
        );
        assert!(tarball.exists(), "失败时必须保留已获取的安装包（FR-1.6）");
    }

    #[tokio::test]
    async fn no_package_manager_yields_install_pm_missing() {
        let tarball = tmp_tarball("no-pm");
        let lister = FakeLister::empty();
        let pm = RecordingPm::new(vec![], vec![None]);
        let e = run(&tarball, &[], &pm, &lister).await.unwrap_err();
        assert_eq!(e.code, crate::error::INSTALL_PM_MISSING);
        assert!(
            e.next_step.contains("npm"),
            "下一步要告诉用户装什么：{}",
            e.next_step
        );
    }
}

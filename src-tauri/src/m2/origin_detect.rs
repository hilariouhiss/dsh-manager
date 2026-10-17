//! M2 安装来源识别：多来源取证、四值判定、依据清单。
//!
//! 本模块只读（FR-2.4）：没有写方法，取证只做列目录 / 读文件 / 看 PATH。
//!
//! 唯一结论类型复用 §3.3.1 冻结在 M1 的 `OriginVerdict`：M1 不得 use M2，
//! 所以结论类型定义在 M1，本模块只做映射，不定义第二份。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tokio::time::timeout;

use crate::m1::version_check::{OriginFacts, OriginVerdict};
use crate::ports::package_manager::{DSH_PACKAGE_NAME, PackageManager, PmInfo, PmKind};

/// 单项来源的时限。包管理器探测覆盖 npm / pnpm / bun 三个来源（按来源数给预算），
/// 其余来源各自一份；整体因此在数秒内返回（FR-2.5、NFR-4）。
pub const PER_SOURCE_TIMEOUT: Duration = Duration::from_secs(3);

/// 官方安装器目录的候选名（大小写不敏感）。配置化，便于按实测修正（§7.3 风险缓解）。
///
/// > 待核实：官方安装器在 Windows 上的确切落盘目录名与可执行文件名（本机未安装官方安装器，无法实测）。
pub const INSTALLER_MARKERS: &[&str] = &["dsh", "DSH", "DeepSeek Harness", "deepseek-harness"];

/// `PackageManager::available()` 一次覆盖的包管理器数量（npm / pnpm / bun）。
const PM_SOURCE_COUNT: u32 = 3;

/// 一条判定依据：「看到了什么、在哪里看到的」（FR-2.2）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub source: String,
    pub path: String,
    pub observed: String,
    pub supports: bool,
}

/// 命令 `detect_origin` 的返回值。`Unidentified` 是**正常返回值**，不是错误。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginReport {
    /// 序列化形状由 M1 的 `OriginVerdict` 决定（外部标签）。
    pub verdict: OriginVerdict,
    pub evidences: Vec<Evidence>,
    pub missing: Vec<String>,
}

/// 一个包管理器的取证结果。
#[derive(Debug, Clone)]
pub struct PmFact {
    pub info: PmInfo,
    /// 该全局目录下 `@deepseek-ai/dsh` 的版本；`None` 表示未安装。
    pub installed: Option<semver::Version>,
}

/// 一次取证的**纯数据**结果。`judge` 只吃它，因此判定逻辑可离线单测。
///
/// 名字不叫 `OriginFacts`：那是 §3.3.1 冻结在 M1 的消费形状（verdict + missing +
/// local_version），本结构只是取证中间态，`gather` 负责把它映射过去。
#[derive(Debug, Clone, Default)]
pub struct ProbeFacts {
    pub pm: Vec<PmFact>,
    /// 未能完成的来源：超时、读失败、找不到包管理器。逐条进 `missing`（FR-2.5）。
    pub source_failures: Vec<String>,
    /// PATH 中解析到的 dsh 启动器。
    pub launcher: Option<PathBuf>,
    /// 启动器同级的 node_modules（存在且含本包时才有值）。
    pub launcher_node_modules: Option<PathBuf>,
    /// 命中的安装器目录。
    pub installer_hit: Option<PathBuf>,
    /// 未能检查的安装器目录。
    pub installer_misses: Vec<String>,
}

fn pm_rank(kind: PmKind) -> u8 {
    match kind {
        PmKind::Npm => 0,
        PmKind::Pnpm => 1,
        PmKind::Bun => 2,
    }
}

/// 路径比较用规范化形式：统一分隔符、统一大小写（Windows 路径不区分大小写）。
fn norm_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").to_ascii_lowercase()
}

/// 纯函数判定：由事实得到唯一结论 + 依据清单 + 缺失项。
pub fn judge(facts: &ProbeFacts) -> OriginReport {
    let mut evidences = Vec::new();
    let mut missing = facts.source_failures.clone();

    for f in &facts.pm {
        let label = f.info.kind.label_zh();
        let observed = match &f.installed {
            Some(v) => format!(
                "该全局目录下有 {DSH_PACKAGE_NAME} {v}（由 {label} {} 管理）",
                f.info.version
            ),
            None => format!("该全局目录下没有 {DSH_PACKAGE_NAME}"),
        };

        evidences.push(Evidence {
            source: format!("包管理器全局目录（{label}）"),
            path: f.info.global_root.display().to_string(),
            observed,
            supports: f.installed.is_some(),
        });
    }

    let mut installed: Vec<&PmFact> = facts.pm.iter().filter(|f| f.installed.is_some()).collect();
    installed.sort_by_key(|f| pm_rank(f.info.kind));

    if installed.len() > 1 {
        let names: Vec<&str> = installed.iter().map(|f| f.info.kind.label_zh()).collect();
        let roots: Vec<String> = installed
            .iter()
            .map(|f| f.info.global_root.display().to_string())
            .collect();
        evidences.push(Evidence {
            source: "多处安装".to_string(),
            path: roots.join("；"),
            observed: format!(
                "检测到 {} 处包管理器全局安装（{}）；本工具按 npm → pnpm → bun 的顺序取第一处作为结论，请确认哪一处是本机实际使用的那一份",
                installed.len(),
                names.join("、")
            ),
            supports: true,
        });
    }

    match &facts.launcher {
        Some(l) => evidences.push(Evidence {
            source: "PATH 中的 dsh 启动器".to_string(),
            path: l.display().to_string(),
            observed: "该路径在 PATH 中可解析到 dsh".to_string(),
            supports: true,
        }),
        None => missing.push("PATH 中没有可解析的 dsh 启动器".to_string()),
    }

    if let Some(nm) = &facts.launcher_node_modules {
        evidences.push(Evidence {
            source: "启动器同级的包目录".to_string(),
            path: nm.display().to_string(),
            observed: format!("启动器所在位置包含 {DSH_PACKAGE_NAME}"),
            supports: true,
        });
    }

    if let Some(dir) = &facts.installer_hit {
        evidences.push(Evidence {
            source: "官方安装器目录".to_string(),
            path: dir.display().to_string(),
            observed: "找到疑似官方安装器的安装目录".to_string(),
            supports: true,
        });
    }
    for m in &facts.installer_misses {
        missing.push(format!("安装器目录不可用：{m}"));
    }

    // 便携版：启动器同级的包目录存在，且**不属于**任何包管理器全局目录、也**不在**安装器目录下。
    // 只有在至少探测到一个包管理器时才能这么说——否则无法排除「本来就是包管理器装的」。
    let portable_root = facts.launcher_node_modules.as_ref().and_then(|nm| {
        let is_pm_root = facts
            .pm
            .iter()
            .any(|f| norm_path(&f.info.global_root) == norm_path(nm));
        let under_installer = facts
            .installer_hit
            .as_ref()
            .is_some_and(|d| norm_path(nm).starts_with(&norm_path(d)));
        let pm_probed = !facts.pm.is_empty();
        if is_pm_root || under_installer || !pm_probed {
            None
        } else {
            Some(nm.clone())
        }
    });
    if let Some(root) = &portable_root {
        evidences.push(Evidence {
            source: "便携版旁证".to_string(),
            path: root.display().to_string(),
            observed: format!("{DSH_PACKAGE_NAME} 位于既非包管理器全局目录、也非安装器目录的位置"),
            supports: true,
        });
    }

    let verdict = if let Some(first) = installed.first() {
        OriginVerdict::PackageManager(first.info.kind)
    } else if facts.installer_hit.is_some() {
        OriginVerdict::OfficialInstaller
    } else if portable_root.is_some() {
        OriginVerdict::Portable
    } else {
        OriginVerdict::Unidentified
    };

    if matches!(verdict, OriginVerdict::Unidentified) && missing.is_empty() {
        missing.push(
            "未取得任何支持性证据：没有包管理器全局安装，PATH 中没有 dsh 启动器，安装器目录也没有命中"
                .to_string(),
        );
    }

    OriginReport {
        verdict,
        evidences,
        missing,
    }
}

/// 取证要看的目录集合。生产用 `for_this_machine()`，测试直接构造。
#[derive(Debug, Clone, Default)]
pub struct OriginPaths {
    /// 安装器目录候选（绝对路径），例如 `%LOCALAPPDATA%\Programs`。
    pub installer_roots: Vec<PathBuf>,
    /// 解析 dsh 启动器时按顺序扫描的目录，通常是 PATH 的各个条目。
    pub path_dirs: Vec<PathBuf>,
}

impl OriginPaths {
    /// 从环境变量推出本机要看的目录。M2 读环境变量取证是设计文档 §2.1 明确允许的。
    pub fn for_this_machine() -> Self {
        let installer_roots = std::env::var_os("LOCALAPPDATA")
            .map(|local| vec![PathBuf::from(local).join("Programs")])
            .unwrap_or_default();
        let path_dirs = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        Self {
            installer_roots,
            path_dirs,
        }
    }
}

/// 在给定目录序列中解析一个可执行文件。**纯文件系统查找：不启动进程、不写任何东西。**
///
/// 与适配层里的同名函数是刻意的重复：设计文档 §2.3 禁止应用层依赖适配层。
fn resolve_in<I>(dirs: I, name: &str) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    for dir in dirs {
        for ext in [".cmd", ".exe", ".bat", ""] {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 逐来源取证。任何单项失败都只记进 `source_failures`，不影响其它来源（FR-2.5）。
///
/// 只做「列目录 / 读文件 / 看 PATH」：端口调用是只读的 available/installed_version，
/// 不调用任何写方法，也不启动进程、不改环境。
async fn probe_sources(
    pm: &dyn PackageManager,
    paths: &OriginPaths,
    per_source: Duration,
) -> ProbeFacts {
    let mut facts = ProbeFacts::default();

    // available() 一次覆盖三种包管理器，适配器内部按 npm → pnpm → bun **顺序**探测
    // （每次探测还要各起一条只读命令）。若把三者挤进一个 per_source，慢机器上会
    // 把「其实有证据」误判成「探测超时」，版本页随之失去结论。
    let available_budget = per_source.saturating_mul(PM_SOURCE_COUNT);
    match timeout(available_budget, pm.available()).await {
        Ok(list) => {
            if list.is_empty() {
                facts.source_failures.push(
                    "未找到可用的包管理器（npm / pnpm / bun），无法排除包管理器安装".to_string(),
                );
            }
            for info in list {
                match timeout(per_source, pm.installed_version(&info)).await {
                    Ok(Ok(installed)) => facts.pm.push(PmFact { info, installed }),
                    Ok(Err(e)) => facts.source_failures.push(format!(
                        "读取 {} 的全局目录失败：{}（{}）",
                        info.kind.label_zh(),
                        e.reason,
                        info.global_root.display()
                    )),
                    Err(_) => facts.source_failures.push(format!(
                        "读取 {} 的全局目录超时（{} 毫秒）",
                        info.kind.label_zh(),
                        per_source.as_millis()
                    )),
                }
            }
        }
        Err(_) => facts.source_failures.push(format!(
            "探测包管理器超时（{} 毫秒）",
            available_budget.as_millis()
        )),
    }

    facts.launcher = resolve_in(paths.path_dirs.iter().cloned(), "dsh");
    if let Some(launcher) = &facts.launcher
        && let Some(dir) = launcher.parent()
    {
        let node_modules = dir.join("node_modules");
        if node_modules
            .join(DSH_PACKAGE_NAME)
            .join("package.json")
            .is_file()
        {
            facts.launcher_node_modules = Some(node_modules);
        }
    }

    for root in &paths.installer_roots {
        if !root.is_dir() {
            facts
                .installer_misses
                .push(format!("{}（目录不存在）", root.display()));
            continue;
        }
        match std::fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if INSTALLER_MARKERS
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(&name))
                    {
                        facts.installer_hit = Some(entry.path());
                    }
                }
            }
            Err(e) => facts
                .installer_misses
                .push(format!("{}（无法读取：{e}）", root.display())),
        }
    }

    facts
}

/// 本机已装版本：按 npm → pnpm → bun 取第一处**已安装**的包管理器报告的版本。
///
/// 与 `judge` 的结论同源，避免出现「判定为 pnpm 安装、local_version 却取 npm 的空值」。
fn local_version_of(facts: &ProbeFacts) -> Option<semver::Version> {
    facts
        .pm
        .iter()
        .filter(|f| f.installed.is_some())
        .min_by_key(|f| pm_rank(f.info.kind))
        .and_then(|f| f.installed.clone())
}

/// 把判定结果映射成 §3.3.1 冻结的 M1 消费形状（verdict + missing + local_version）。
fn facts_from(report: &OriginReport, probe: &ProbeFacts) -> OriginFacts {
    OriginFacts {
        verdict: report.verdict,
        missing: report.missing.clone(),
        local_version: local_version_of(probe),
    }
}

/// 一次取证同时产出「给人看的依据清单」与「给 M1 用的事实」。
///
/// `detect_origin` 用同一次探测写共享状态并返回报告，避免把本机探测跑两遍。
pub async fn detect_and_gather(
    pm: &dyn PackageManager,
    paths: &OriginPaths,
    per_source: Duration,
) -> (OriginReport, OriginFacts) {
    let probe = probe_sources(pm, paths, per_source).await;
    let report = judge(&probe);
    let facts = facts_from(&report, &probe);
    (report, facts)
}

/// 命令入口：取证 + 判定。**返回值永远是 `OriginReport`**——`Unidentified` 不是错误。
pub async fn detect(
    pm: &dyn PackageManager,
    paths: &OriginPaths,
    per_source: Duration,
) -> OriginReport {
    detect_and_gather(pm, paths, per_source).await.0
}

/// 取证并映射成 M1 消费的事实。`check_updates` 每次调用前都刷新它，保证安装后版本不为旧值。
pub async fn gather(
    pm: &dyn PackageManager,
    paths: &OriginPaths,
    per_source: Duration,
) -> OriginFacts {
    detect_and_gather(pm, paths, per_source).await.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_uses_the_frozen_m1_wire_shape() {
        // 外部标签：带载荷的变体变成单键对象，其余变体是字符串（与 m1::version_check 一致）。
        assert_eq!(
            serde_json::to_value(OriginVerdict::PackageManager(PmKind::Npm)).unwrap(),
            serde_json::json!({ "packageManager": "npm" })
        );
        assert_eq!(
            serde_json::to_value(OriginVerdict::OfficialInstaller).unwrap(),
            serde_json::json!("officialInstaller")
        );
        assert_eq!(
            serde_json::to_value(OriginVerdict::Portable).unwrap(),
            serde_json::json!("portable")
        );
        assert_eq!(
            serde_json::to_value(OriginVerdict::Unidentified).unwrap(),
            serde_json::json!("unidentified")
        );
    }

    #[test]
    fn report_serializes_its_three_fields() {
        let report = OriginReport {
            verdict: OriginVerdict::Unidentified,
            evidences: vec![Evidence {
                source: "PATH".to_string(),
                path: r"C:\Users\me\AppData\Roaming\npm\dsh.cmd".to_string(),
                observed: "可解析到 dsh".to_string(),
                supports: true,
            }],
            missing: vec!["没有任何可用来源".to_string()],
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["verdict"], "unidentified");
        assert_eq!(json["evidences"][0]["supports"], true);
        assert_eq!(json["evidences"][0]["source"], "PATH");
        assert_eq!(json["missing"][0], "没有任何可用来源");
    }

    use crate::adapters::fake::fake_pm::fake_pm_info;

    const NPM_ROOT: &str = r"C:\Users\me\AppData\Roaming\npm\node_modules";
    const PNPM_ROOT: &str = r"C:\Users\me\AppData\Local\pnpm\global\5";

    fn fact(kind: PmKind, root: &str, installed: Option<&str>) -> PmFact {
        PmFact {
            info: fake_pm_info(kind, PathBuf::from(root)),
            installed: installed.map(|v| semver::Version::parse(v).unwrap()),
        }
    }

    #[test]
    fn npm_global_install_is_the_verdict() {
        let facts = ProbeFacts {
            pm: vec![fact(PmKind::Npm, NPM_ROOT, Some("0.2.1-alpha.4"))],
            launcher: Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\dsh.cmd")),
            ..Default::default()
        };
        let report = judge(&facts);
        assert_eq!(report.verdict, OriginVerdict::PackageManager(PmKind::Npm));
        assert!(
            report
                .evidences
                .iter()
                .any(|e| e.supports && e.path.contains("node_modules"))
        );
    }

    #[test]
    fn official_installer_is_recognized() {
        let facts = ProbeFacts {
            installer_hit: Some(PathBuf::from(r"C:\Users\me\AppData\Local\Programs\DSH")),
            ..Default::default()
        };
        assert_eq!(judge(&facts).verdict, OriginVerdict::OfficialInstaller);
    }

    #[test]
    fn portable_install_is_recognized() {
        let facts = ProbeFacts {
            pm: vec![fact(PmKind::Npm, NPM_ROOT, None)],
            launcher: Some(PathBuf::from(r"D:\tools\dsh\dsh.cmd")),
            launcher_node_modules: Some(PathBuf::from(r"D:\tools\dsh\node_modules")),
            ..Default::default()
        };
        assert_eq!(judge(&facts).verdict, OriginVerdict::Portable);
    }

    #[test]
    fn launcher_inside_a_pm_global_root_is_not_portable() {
        let facts = ProbeFacts {
            pm: vec![fact(PmKind::Npm, NPM_ROOT, Some("0.2.1-alpha.4"))],
            launcher: Some(PathBuf::from(r"C:\Users\me\AppData\Roaming\npm\dsh.cmd")),
            launcher_node_modules: Some(PathBuf::from(NPM_ROOT)),
            ..Default::default()
        };
        assert_eq!(
            judge(&facts).verdict,
            OriginVerdict::PackageManager(PmKind::Npm)
        );
    }

    #[test]
    fn insufficient_evidence_is_unidentified_with_missing_items() {
        let facts = ProbeFacts {
            pm: vec![fact(PmKind::Npm, NPM_ROOT, None)],
            installer_misses: vec![r"C:\Users\me\AppData\Local\Programs（目录不存在）".to_string()],
            ..Default::default()
        };
        let report = judge(&facts);
        assert_eq!(report.verdict, OriginVerdict::Unidentified);
        assert!(
            !report.missing.is_empty(),
            "依据不足时必须说明缺什么（FR-2.3）"
        );
    }

    #[test]
    fn launcher_without_any_pm_probe_is_not_portable() {
        let facts = ProbeFacts {
            launcher: Some(PathBuf::from(r"D:\tools\dsh\dsh.cmd")),
            launcher_node_modules: Some(PathBuf::from(r"D:\tools\dsh\node_modules")),
            source_failures: vec!["未找到可用的包管理器（npm / pnpm / bun）".to_string()],
            ..Default::default()
        };
        let report = judge(&facts);
        assert_eq!(
            report.verdict,
            OriginVerdict::Unidentified,
            "没有可用包管理器时不得断言便携版"
        );
        assert!(report.missing.iter().any(|m| m.contains("包管理器")));
    }

    #[test]
    fn multiple_installs_are_listed_and_npm_wins() {
        let facts = ProbeFacts {
            pm: vec![
                fact(PmKind::Pnpm, PNPM_ROOT, Some("0.2.0")),
                fact(PmKind::Npm, NPM_ROOT, Some("0.2.1-alpha.4")),
            ],
            ..Default::default()
        };
        let report = judge(&facts);
        assert_eq!(
            report.verdict,
            OriginVerdict::PackageManager(PmKind::Npm),
            "按 npm → pnpm → bun 取第一处"
        );
        assert!(
            report.evidences.iter().any(|e| e.source == "多处安装"),
            "多处安装要列出来让用户选（Q3）"
        );
    }

    use crate::adapters::fake::fake_pm::FakePm;
    use std::time::Instant;

    fn temp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dsh-manager-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 递归快照（类型 + 字节数 + 路径），用于断言「识别过程没有改任何东西」（FR-2.4）。
    fn snapshot_tree(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(format!(
                    "{}|{}|{}",
                    if p.is_dir() { "dir" } else { "file" },
                    len,
                    p.display()
                ));
                if p.is_dir() {
                    stack.push(p);
                }
            }
        }
        out.sort();
        out
    }

    /// 在临时目录里造出一个完整的「npm 全局装了 dsh」现场。
    fn npm_global_fixture(root: &Path) -> (PathBuf, PathBuf) {
        let bin = root.join("npm");
        let global = bin.join("node_modules");
        let pkg = global.join(DSH_PACKAGE_NAME);
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"version":"0.2.1-alpha.4"}"#).unwrap();
        std::fs::write(bin.join("dsh.cmd"), "@echo off\r\n").unwrap();
        (bin, global)
    }

    #[tokio::test]
    async fn detect_origin_never_writes() {
        let root = temp_dir("origin-zero-write");
        let (bin, global) = npm_global_fixture(&root);
        let before = snapshot_tree(&root);

        let pm = FakePm::new(vec![fake_pm_info(PmKind::Npm, global.clone())]).with_installed(
            PmKind::Npm,
            Some(semver::Version::parse("0.2.1-alpha.4").unwrap()),
        );
        let paths = OriginPaths {
            installer_roots: vec![root.join("Programs")],
            path_dirs: vec![bin.clone()],
        };

        let report = detect(&pm, &paths, Duration::from_millis(200)).await;

        assert_eq!(report.verdict, OriginVerdict::PackageManager(PmKind::Npm));
        assert!(
            pm.write_calls().is_empty(),
            "识别路径出现写调用：{:?}",
            pm.write_calls()
        );
        assert_eq!(
            pm.calls(),
            vec!["available", "installed_version:npm"],
            "识别只允许调用只读端口方法"
        );
        assert_eq!(
            snapshot_tree(&root),
            before,
            "识别过程修改了本机文件（违反 FR-2.4）"
        );
    }

    #[tokio::test]
    async fn one_failed_source_is_skipped_and_recorded() {
        let root = temp_dir("origin-one-failure");
        let (_bin, global) = npm_global_fixture(&root);

        let pm = FakePm::new(vec![
            fake_pm_info(PmKind::Npm, global.clone()),
            fake_pm_info(PmKind::Pnpm, root.join("pnpm").join("global")),
        ])
        .with_installed(
            PmKind::Npm,
            Some(semver::Version::parse("0.2.1-alpha.4").unwrap()),
        )
        .with_failure(PmKind::Pnpm);

        let paths = OriginPaths {
            installer_roots: vec![root.join("Programs")],
            path_dirs: Vec::new(),
        };
        let report = detect(&pm, &paths, Duration::from_millis(200)).await;

        assert_eq!(
            report.verdict,
            OriginVerdict::PackageManager(PmKind::Npm),
            "一个来源失败不得影响结论"
        );
        assert!(
            report.missing.iter().any(|m| m.contains("pnpm")),
            "失败来源要记进 missing：{:?}",
            report.missing
        );
    }

    #[tokio::test]
    async fn a_timing_out_source_is_abandoned_not_awaited() {
        let root = temp_dir("origin-timeout");
        let pm = FakePm::new(vec![fake_pm_info(PmKind::Npm, root.join("npm"))])
            .with_delay(Duration::from_millis(600));
        let paths = OriginPaths::default();

        let started = Instant::now();
        let report = detect(&pm, &paths, Duration::from_millis(50)).await;

        assert!(
            started.elapsed() < Duration::from_millis(400),
            "超时来源必须被放弃，不能干等（FR-2.5）"
        );
        assert_eq!(report.verdict, OriginVerdict::Unidentified);
        assert!(
            report.missing.iter().any(|m| m.contains("超时")),
            "缺什么要写清楚：{:?}",
            report.missing
        );
    }

    #[tokio::test]
    async fn installer_directory_hit_is_detected() {
        let root = temp_dir("origin-installer");
        let programs = root.join("Programs");
        std::fs::create_dir_all(programs.join("DSH")).unwrap();

        let pm = FakePm::new(Vec::new());
        let paths = OriginPaths {
            installer_roots: vec![programs],
            path_dirs: Vec::new(),
        };
        let report = detect(&pm, &paths, Duration::from_millis(200)).await;

        assert_eq!(report.verdict, OriginVerdict::OfficialInstaller);
        assert!(report.evidences.iter().any(|e| e.path.contains("DSH")));
    }

    /// 裁定 2：gather 要把中间事实完整映射成 M1 消费形状——
    /// 结论、缺失项，以及**按 npm → pnpm → bun 顺序**取第一处已安装的版本。
    #[tokio::test]
    async fn gather_maps_verdict_missing_and_local_version() {
        let root = temp_dir("origin-gather-facts");
        let (_bin, global) = npm_global_fixture(&root);
        let pm = FakePm::new(vec![
            fake_pm_info(PmKind::Pnpm, root.join("pnpm").join("global")),
            fake_pm_info(PmKind::Npm, global),
        ])
        .with_installed(PmKind::Pnpm, Some(semver::Version::parse("0.2.0").unwrap()))
        .with_installed(
            PmKind::Npm,
            Some(semver::Version::parse("0.2.1-alpha.4").unwrap()),
        );

        let paths = OriginPaths {
            installer_roots: Vec::new(),
            path_dirs: Vec::new(),
        };
        let facts = gather(&pm, &paths, Duration::from_millis(200)).await;

        assert_eq!(facts.verdict, OriginVerdict::PackageManager(PmKind::Npm));
        assert_eq!(
            facts.local_version,
            Some(semver::Version::parse("0.2.1-alpha.4").unwrap()),
            "local_version 必须与结论同源（npm 优先于 pnpm）"
        );
        assert!(
            facts.missing.iter().any(|m| m.contains("启动器")),
            "missing 要随报告一起映射：{:?}",
            facts.missing
        );
        assert!(
            pm.write_calls().is_empty(),
            "映射事实同样不得产生写调用：{:?}",
            pm.write_calls()
        );
    }

    /// §5 T-O2 验收（E2E-3）：本机结论为「包管理器安装（npm）」，依据指向具体路径。
    #[tokio::test]
    #[ignore = "本机验收：需要已安装 npm 与全局 @deepseek-ai/dsh"]
    async fn detects_the_real_npm_global_install_on_this_machine() {
        let pm = crate::adapters::AllPackageManagers;
        let paths = OriginPaths::for_this_machine();
        let report = detect(&pm, &paths, PER_SOURCE_TIMEOUT).await;
        eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
        assert_eq!(report.verdict, OriginVerdict::PackageManager(PmKind::Npm));
        assert!(
            report
                .evidences
                .iter()
                .any(|e| e.supports && e.path.to_ascii_lowercase().contains("npm")),
            "依据清单必须指向具体路径"
        );

        let facts = gather(&pm, &paths, PER_SOURCE_TIMEOUT).await;
        eprintln!("local_version = {:?}", facts.local_version);
        assert!(facts.local_version.is_some(), "本机已装版本不得丢失");
        assert!(facts.version_applicable(), "包管理器安装必须让版本对比适用");
    }
}

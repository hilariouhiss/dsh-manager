//! 诊断包导出（NFR-6）。内容清单固定，且不含任何凭据类信息（主计划 §1.3 红线 5）。

use std::fs;
use std::path::Path;

use crate::error::{self, AppError, CmdResult};
use crate::m0::oplog::OplogEntry;
use crate::m0::paths::DataPaths;
use crate::m0::settings::Settings;

/// 进入诊断包的最近操作记录条数上限（主计划 T-S5 要点：最近 200 条）。
pub const OPLOG_TAIL: usize = 200;

/// 诊断包的全部输入。本机 DSH 版本与来源结论由调用方注入：它们分别来自版本检查与
/// 来源识别，M0 不自行判定，也绝不猜测。
pub struct DiagnosticsInput<'a> {
    pub paths: &'a DataPaths,
    pub settings: &'a Settings,
    pub entries: &'a [OplogEntry],
    pub local_dsh_version: Option<String>,
    pub origin_verdict: Option<String>,
}

/// 组装可直接阅读的诊断文本（Markdown）。只写固定清单里的字段。
pub fn build_report(input: &DiagnosticsInput<'_>) -> String {
    let mut out = String::new();
    out.push_str("# DSH Manager 诊断报告\n\n");

    out.push_str("## 运行环境\n\n");
    out.push_str(&format!("- 应用版本：{}\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!("- 操作系统：{}\n", os_version()));
    out.push_str(&format!(
        "- 数据目录：{}\n",
        input.paths.app_data_dir.display()
    ));
    out.push_str(&format!(
        "- 缓存目录：{}\n",
        input.paths.cache_dir.display()
    ));
    out.push_str(&format!("- 临时目录：{}\n", input.paths.temp_dir.display()));

    out.push_str("\n## 本机 DSH\n\n");
    out.push_str(&format!(
        "- 本机版本：{}\n",
        input.local_dsh_version.as_deref().unwrap_or("未取得")
    ));
    out.push_str(&format!(
        "- 来源结论：{}\n",
        input.origin_verdict.as_deref().unwrap_or("未取得")
    ));

    out.push_str("\n## 设置快照\n\n");
    out.push_str(&serde_json::to_string_pretty(input.settings).unwrap_or_else(|_| "{}".to_owned()));

    out.push_str("\n\n## 最近操作记录\n\n");
    out.push_str(&format!(
        "共 {} 条（上限 {} 条），最新在前\n\n",
        input.entries.len(),
        OPLOG_TAIL
    ));
    for entry in input.entries {
        out.push_str(&serde_json::to_string(entry).unwrap_or_else(|_| "{}".to_owned()));
        out.push('\n');
    }
    out
}

/// 写盘并返回写入字节数（主计划 §3.4 命令 15 的出参 bytesWritten）。
pub fn export(input: &DiagnosticsInput<'_>, target: &Path) -> CmdResult<u64> {
    let text = build_report(input);
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| io_error("创建导出目录", parent, e))?;
    }
    fs::write(target, &text).map_err(|e| io_error("写入诊断文件", target, e))?;
    Ok(text.len() as u64)
}

/// 操作系统长版本串（sysinfo 提供，不自建系统调用）。
fn os_version() -> String {
    sysinfo::System::long_os_version().unwrap_or_else(|| "无法取得".to_owned())
}

fn io_error(action: &str, path: &Path, e: std::io::Error) -> AppError {
    AppError::new(
        error::PATH_PERMISSION_DENIED,
        format!("{action}失败：{}", path.display()),
        "确认目标目录存在且当前用户有写权限，然后重试。",
    )
    .with_raw(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m0::paths::temp_root;

    fn sample(tag: &str) -> (DataPaths, Settings, Vec<OplogEntry>) {
        let paths = DataPaths::under_root(&temp_root(tag));
        paths.ensure_dirs().unwrap();
        let entries = vec![
            OplogEntry::ok("settings.update", "settings.json"),
            OplogEntry::fail(
                "settings.load",
                "settings.json",
                error::SETTINGS_CORRUPT,
                "JSON 解析失败",
            ),
        ];
        (paths, Settings::default(), entries)
    }

    #[test]
    fn report_contains_required_sections() {
        let (paths, settings, entries) = sample("diagnostics-report");
        let input = DiagnosticsInput {
            paths: &paths,
            settings: &settings,
            entries: &entries,
            local_dsh_version: Some("0.2.1-alpha.4".to_owned()),
            origin_verdict: Some("包管理器安装（npm）".to_owned()),
        };
        let report = build_report(&input);

        for needle in [
            "# DSH Manager 诊断报告",
            "## 运行环境",
            "## 本机 DSH",
            "## 设置快照",
            "## 最近操作记录",
        ] {
            assert!(report.contains(needle), "缺小节：{needle}");
        }
        assert!(report.contains(env!("CARGO_PKG_VERSION")));
        let os_line = report
            .lines()
            .find(|line| line.starts_with("- 操作系统："))
            .expect("应有操作系统一行");
        assert!(
            os_line.len() > "- 操作系统：".len(),
            "操作系统版本不得为空：{os_line}"
        );
        assert!(report.contains("0.2.1-alpha.4"));
        assert!(report.contains("包管理器安装（npm）"));
        assert!(report.contains("\"outputBufferLines\": 5000"));
        assert!(report.contains("settings.corrupt"));
    }

    #[test]
    fn missing_dsh_facts_are_reported_as_not_available() {
        let (paths, settings, entries) = sample("diagnostics-missing");
        let input = DiagnosticsInput {
            paths: &paths,
            settings: &settings,
            entries: &entries,
            local_dsh_version: None,
            origin_verdict: None,
        };
        let report = build_report(&input);
        assert_eq!(report.matches("未取得").count(), 2);
    }

    #[test]
    fn report_never_contains_credential_words() {
        // 临时目录名本身不能含被禁词，否则数据目录路径会把该词带进报告。
        let (paths, settings, entries) = sample("diagnostics-sensitive");
        let input = DiagnosticsInput {
            paths: &paths,
            settings: &settings,
            entries: &entries,
            local_dsh_version: None,
            origin_verdict: None,
        };
        let lowered = build_report(&input).to_lowercase();
        for forbidden in ["credential", "token", "secret", "apikey"] {
            assert!(!lowered.contains(forbidden), "诊断内容不得包含 {forbidden}");
        }
    }

    #[test]
    fn export_writes_file_and_reports_bytes() {
        let (paths, settings, entries) = sample("diagnostics-export");
        let input = DiagnosticsInput {
            paths: &paths,
            settings: &settings,
            entries: &entries,
            local_dsh_version: None,
            origin_verdict: None,
        };
        let target = paths.temp_dir.join("diagnostics.md");
        let bytes = export(&input, &target).unwrap();
        let text = fs::read_to_string(&target).unwrap();
        assert_eq!(bytes, text.len() as u64);
        assert!(text.starts_with("# DSH Manager 诊断报告"));
    }
}

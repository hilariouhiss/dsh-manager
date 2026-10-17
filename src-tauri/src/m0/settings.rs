//! 设置读写、默认值、字段级校验与损坏回落（主计划 §3.6、§3.7）。
//!
//! 落盘位置由 m0::paths::DataPaths::settings_file() 给出；本模块不硬编码任何目录。
//! 损坏回落**不覆盖原文件**：保留现场供排查，下一次 update_settings 会写入合法内容。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{self, AppError, CmdResult};
use crate::m0::logging::RollPolicy;
use crate::m0::oplog::{Oplog, OplogEntry};

/// 启动行为（主计划 §3.7，FR-5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutostartMode {
    Tray,
    Window,
}

/// 设置全表（主计划 §3.7，14 项，字段名 camelCase，与落盘及 IPC 同名）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub auto_channel: Option<String>,
    pub auto_install: bool,
    pub confirm_before_install: bool,
    pub launch_args: Vec<String>,
    pub work_dir: String,
    pub auto_open_browser: bool,
    pub auto_scroll: bool,
    pub output_buffer_lines: u32,
    // serde 的 camelCase 会把 MiB 变成 Mib，与 §3.7 冻结的键名不符，故显式指定。
    #[serde(rename = "logMaxMiB")]
    pub log_max_mib: u32,
    pub log_keep_files: u32,
    #[serde(rename = "oplogMaxMiB")]
    pub oplog_max_mib: u32,
    pub oplog_keep_files: u32,
    pub stop_instance_on_exit: bool,
    pub autostart_mode: AutostartMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_channel: None,
            auto_install: false,
            confirm_before_install: true,
            launch_args: vec!["--no-open".to_owned()],
            work_dir: default_work_dir(),
            auto_open_browser: true,
            auto_scroll: true,
            output_buffer_lines: 5000,
            log_max_mib: 5,
            log_keep_files: 3,
            oplog_max_mib: 2,
            oplog_keep_files: 3,
            stop_instance_on_exit: true,
            autostart_mode: AutostartMode::Tray,
        }
    }
}

/// workDir 默认值：%USERPROFILE%（主计划 §3.7）。读不到时退到系统盘根目录，不阻断启动。
pub fn default_work_dir() -> String {
    std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_owned())
}

/// 读取设置的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct LoadOutcome {
    /// 实际生效的设置
    pub settings: Settings,
    /// 触发回落或被拒绝的原因；None 表示文件完全合法（或原本不存在、刚写入默认值）
    pub corrupt: Option<String>,
}

/// 读取设置：文件不存在则写入默认值；存在则逐字段解析，任何回落都记入 corrupt。
pub fn load_or_init(path: &Path) -> CmdResult<LoadOutcome> {
    if !path.exists() {
        let settings = Settings::default();
        save(path, &settings)?;
        return Ok(LoadOutcome {
            settings,
            corrupt: None,
        });
    }
    let text = fs::read_to_string(path).map_err(|e| {
        AppError::new(
            error::PATH_PERMISSION_DENIED,
            format!("无法读取设置文件：{}", path.display()),
            "确认该文件未被其他程序占用且当前用户有读取权限，然后重试。",
        )
        .with_raw(e.to_string())
    })?;
    Ok(parse(&text))
}

/// 纯解析：不触碰文件系统，便于单测。
pub fn parse(text: &str) -> LoadOutcome {
    let defaults = Settings::default();
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            return LoadOutcome {
                settings: defaults,
                corrupt: Some(format!("JSON 解析失败：{e}")),
            };
        }
    };
    let Value::Object(map) = value else {
        return LoadOutcome {
            settings: defaults,
            corrupt: Some("设置文件的根不是 JSON 对象".to_owned()),
        };
    };

    let mut problems = Vec::new();
    let mut settings = Settings {
        auto_channel: field(
            &map,
            "autoChannel",
            defaults.auto_channel.clone(),
            &mut problems,
        ),
        auto_install: field(&map, "autoInstall", defaults.auto_install, &mut problems),
        confirm_before_install: field(
            &map,
            "confirmBeforeInstall",
            defaults.confirm_before_install,
            &mut problems,
        ),
        launch_args: field(
            &map,
            "launchArgs",
            defaults.launch_args.clone(),
            &mut problems,
        ),
        work_dir: field(&map, "workDir", defaults.work_dir.clone(), &mut problems),
        auto_open_browser: field(
            &map,
            "autoOpenBrowser",
            defaults.auto_open_browser,
            &mut problems,
        ),
        auto_scroll: field(&map, "autoScroll", defaults.auto_scroll, &mut problems),
        output_buffer_lines: field(
            &map,
            "outputBufferLines",
            defaults.output_buffer_lines,
            &mut problems,
        ),
        log_max_mib: field(&map, "logMaxMiB", defaults.log_max_mib, &mut problems),
        log_keep_files: field(&map, "logKeepFiles", defaults.log_keep_files, &mut problems),
        oplog_max_mib: field(&map, "oplogMaxMiB", defaults.oplog_max_mib, &mut problems),
        oplog_keep_files: field(
            &map,
            "oplogKeepFiles",
            defaults.oplog_keep_files,
            &mut problems,
        ),
        stop_instance_on_exit: field(
            &map,
            "stopInstanceOnExit",
            defaults.stop_instance_on_exit,
            &mut problems,
        ),
        autostart_mode: field(
            &map,
            "autostartMode",
            defaults.autostart_mode,
            &mut problems,
        ),
    };
    validate(&mut settings, &defaults, &mut problems);
    LoadOutcome {
        settings,
        corrupt: if problems.is_empty() {
            None
        } else {
            Some(problems.join("；"))
        },
    }
}

/// 应用部分补丁：只改出现的键，未出现的键保持原值。
/// 返回被拒绝或被回落的字段说明（空表示全部生效）。
pub fn apply_patch(settings: &mut Settings, patch: &Value) -> Vec<String> {
    let defaults = Settings::default();
    let mut problems = Vec::new();
    let Value::Object(map) = patch else {
        return vec!["补丁不是 JSON 对象，已全部忽略".to_owned()];
    };

    patch_field(
        map,
        "autoChannel",
        &mut settings.auto_channel,
        &mut problems,
    );
    patch_field(
        map,
        "autoInstall",
        &mut settings.auto_install,
        &mut problems,
    );
    patch_field(
        map,
        "confirmBeforeInstall",
        &mut settings.confirm_before_install,
        &mut problems,
    );
    patch_field(map, "launchArgs", &mut settings.launch_args, &mut problems);
    patch_field(map, "workDir", &mut settings.work_dir, &mut problems);
    patch_field(
        map,
        "autoOpenBrowser",
        &mut settings.auto_open_browser,
        &mut problems,
    );
    patch_field(map, "autoScroll", &mut settings.auto_scroll, &mut problems);
    patch_field(
        map,
        "outputBufferLines",
        &mut settings.output_buffer_lines,
        &mut problems,
    );
    patch_field(map, "logMaxMiB", &mut settings.log_max_mib, &mut problems);
    patch_field(
        map,
        "logKeepFiles",
        &mut settings.log_keep_files,
        &mut problems,
    );
    patch_field(
        map,
        "oplogMaxMiB",
        &mut settings.oplog_max_mib,
        &mut problems,
    );
    patch_field(
        map,
        "oplogKeepFiles",
        &mut settings.oplog_keep_files,
        &mut problems,
    );
    patch_field(
        map,
        "stopInstanceOnExit",
        &mut settings.stop_instance_on_exit,
        &mut problems,
    );
    patch_field(
        map,
        "autostartMode",
        &mut settings.autostart_mode,
        &mut problems,
    );

    validate(settings, &defaults, &mut problems);
    problems
}

/// 写盘。设置内容是给人看的（NFR-6），因此用 pretty JSON。
pub fn save(path: &Path, settings: &Settings) -> CmdResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            AppError::new(
                error::PATH_PERMISSION_DENIED,
                format!("无法创建设置目录：{}", parent.display()),
                "确认当前用户对该目录有写权限，然后重试。",
            )
            .with_raw(e.to_string())
        })?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| {
        AppError::new(
            error::SETTINGS_CORRUPT,
            "设置内容无法序列化",
            "这是程序内部错误，请导出诊断并反馈。",
        )
        .with_raw(e.to_string())
    })?;
    fs::write(path, text).map_err(|e| {
        AppError::new(
            error::PATH_PERMISSION_DENIED,
            format!("无法写入设置文件：{}", path.display()),
            "确认该文件未被其他程序占用且当前用户有写权限，然后重试。",
        )
        .with_raw(e.to_string())
    })
}

/// 设置文件损坏时记一条操作记录（§3.7「损坏回落」的可追溯证据）。
pub fn record_corrupt(
    oplog: &Oplog,
    settings_file: &Path,
    raw: &str,
    policy: RollPolicy,
) -> CmdResult<OplogEntry> {
    let target = settings_file.display().to_string();
    let entry = OplogEntry::fail("settings.load", &target, error::SETTINGS_CORRUPT, raw);
    oplog.append(&entry, policy)?;
    Ok(entry)
}

/// 设置变更记一条操作记录（旧值 → 新值，设计文档 §7.2「设置的可追溯」）。
pub fn record_change(
    oplog: &Oplog,
    settings_file: &Path,
    before: &Settings,
    after: &Settings,
    policy: RollPolicy,
) -> CmdResult<OplogEntry> {
    let raw = format!(
        "{} → {}",
        serde_json::to_string(before).unwrap_or_default(),
        serde_json::to_string(after).unwrap_or_default()
    );
    let target = settings_file.display().to_string();
    let mut entry = OplogEntry::ok("settings.update", &target);
    entry.raw = Some(raw);
    oplog.append(&entry, policy)?;
    Ok(entry)
}

/// 设置的唯一入口（主计划 §3.3.2 冻结了 `AppState.settings: Arc<SettingsStore>`）。
///
/// 它同时承担三件事：持有内存快照、承载启动时发现的损坏原因、把补丁的校验与落盘串起来。
#[derive(Debug)]
pub struct SettingsStore {
    path: PathBuf,
    current: Mutex<Settings>,
    startup_problem: Option<String>,
}

impl SettingsStore {
    /// 读取设置；文件不存在则写入默认值。损坏原因留给调用方记操作记录（§3.7）。
    pub fn load_or_init(path: PathBuf) -> CmdResult<Self> {
        let outcome = load_or_init(&path)?;
        Ok(Self {
            path,
            current: Mutex::new(outcome.settings),
            startup_problem: outcome.corrupt,
        })
    }

    /// 启动时的损坏原因；None 表示文件完全合法（或原本不存在）。
    pub fn startup_problem(&self) -> Option<&str> {
        self.startup_problem.as_deref()
    }

    pub fn file(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> Settings {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// 逐字段校验 → 落盘 → 替换内存值；返回生效值与被回落/被拒绝的字段说明。
    pub fn apply_patch(&self, patch: &Value) -> CmdResult<(Settings, Vec<String>)> {
        let mut guard = self
            .current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = guard.clone();
        let problems = apply_patch(&mut next, patch);
        save(&self.path, &next)?;
        *guard = next.clone();
        Ok((next, problems))
    }
}

/// 逐字段取值：键不存在 → 默认值（不算问题）；键存在但类型不符 → 默认值（记问题）。
fn field<T: serde::de::DeserializeOwned>(
    map: &Map<String, Value>,
    key: &str,
    fallback: T,
    problems: &mut Vec<String>,
) -> T {
    match map.get(key) {
        None => fallback,
        Some(value) => match serde_json::from_value::<T>(value.clone()) {
            Ok(parsed) => parsed,
            Err(e) => {
                problems.push(format!("{key} 的值不合法（{e}），已回落默认值"));
                fallback
            }
        },
    }
}

/// 补丁逐字段赋值：类型不符时**保持原值**并记问题（§3.2 没有「设置非法」错误码，故不整体失败）。
fn patch_field<T: serde::de::DeserializeOwned>(
    map: &Map<String, Value>,
    key: &str,
    target: &mut T,
    problems: &mut Vec<String>,
) {
    if let Some(value) = map.get(key) {
        match serde_json::from_value::<T>(value.clone()) {
            Ok(parsed) => *target = parsed,
            Err(e) => problems.push(format!("{key} 的值类型不合法（{e}），已保持原值")),
        }
    }
}

/// 数值区间与 workDir 的兜底校验（主计划 §3.7 校验规则）。
fn validate(settings: &mut Settings, defaults: &Settings, problems: &mut Vec<String>) {
    settings.output_buffer_lines = clamp_u32(
        settings.output_buffer_lines,
        1,
        100_000,
        "outputBufferLines",
        defaults.output_buffer_lines,
        problems,
    );
    settings.log_max_mib = clamp_u32(
        settings.log_max_mib,
        1,
        1024,
        "logMaxMiB",
        defaults.log_max_mib,
        problems,
    );
    settings.log_keep_files = clamp_u32(
        settings.log_keep_files,
        1,
        50,
        "logKeepFiles",
        defaults.log_keep_files,
        problems,
    );
    settings.oplog_max_mib = clamp_u32(
        settings.oplog_max_mib,
        1,
        1024,
        "oplogMaxMiB",
        defaults.oplog_max_mib,
        problems,
    );
    settings.oplog_keep_files = clamp_u32(
        settings.oplog_keep_files,
        1,
        50,
        "oplogKeepFiles",
        defaults.oplog_keep_files,
        problems,
    );
    if settings.work_dir.trim().is_empty() {
        problems.push("workDir 为空，已回落默认值".to_owned());
        settings.work_dir = defaults.work_dir.clone();
    }
}

fn clamp_u32(
    value: u32,
    min: u32,
    max: u32,
    key: &str,
    fallback: u32,
    problems: &mut Vec<String>,
) -> u32 {
    if (min..=max).contains(&value) {
        return value;
    }
    problems.push(format!(
        "{key} 的值 {value} 超出 [{min}, {max}]，已回落默认值 {fallback}"
    ));
    fallback
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::m0::paths::{DataPaths, temp_root};

    #[test]
    fn defaults_when_file_missing_and_file_is_created() {
        let paths = DataPaths::under_root(&temp_root("settings-defaults"));
        paths.ensure_dirs().unwrap();
        let outcome = load_or_init(&paths.settings_file()).unwrap();
        assert_eq!(outcome.corrupt, None);
        assert_eq!(outcome.settings.auto_channel, None);
        assert_eq!(outcome.settings.auto_install, false);
        assert_eq!(outcome.settings.confirm_before_install, true);
        assert_eq!(outcome.settings.launch_args, vec!["--no-open".to_owned()]);
        assert_eq!(outcome.settings.work_dir, default_work_dir());
        assert_eq!(outcome.settings.auto_open_browser, true);
        assert_eq!(outcome.settings.auto_scroll, true);
        assert_eq!(outcome.settings.output_buffer_lines, 5000);
        assert_eq!(outcome.settings.log_max_mib, 5);
        assert_eq!(outcome.settings.log_keep_files, 3);
        assert_eq!(outcome.settings.oplog_max_mib, 2);
        assert_eq!(outcome.settings.oplog_keep_files, 3);
        assert_eq!(outcome.settings.stop_instance_on_exit, true);
        assert_eq!(outcome.settings.autostart_mode, AutostartMode::Tray);
        // 「首次启动生成默认 settings.json」：文件已落盘且再次读取完全一致
        let second = load_or_init(&paths.settings_file()).unwrap();
        assert_eq!(second.corrupt, None);
        assert_eq!(second.settings, outcome.settings);
    }

    #[test]
    fn fallback_on_invalid_json() {
        let outcome = parse("{ 这不是 JSON");
        assert_eq!(outcome.settings, Settings::default());
        let reason = outcome.corrupt.expect("应记录回落原因");
        assert!(reason.contains("JSON 解析失败"), "实际：{reason}");
    }

    #[test]
    fn per_field_type_mismatch_falls_back_only_that_field() {
        let outcome = parse("{\"autoScroll\": false, \"outputBufferLines\": \"many\"}");
        assert_eq!(
            outcome.settings.output_buffer_lines, 5000,
            "类型不符应回落默认值"
        );
        assert_eq!(outcome.settings.auto_scroll, false, "合法字段必须保留");
        assert_eq!(outcome.settings.confirm_before_install, true);
        assert!(outcome.corrupt.unwrap().contains("outputBufferLines"));
    }

    #[test]
    fn out_of_range_in_file_falls_back_to_default() {
        let outcome = parse("{\"logMaxMiB\": 4096, \"logKeepFiles\": 0}");
        assert_eq!(outcome.settings.log_max_mib, 5);
        assert_eq!(outcome.settings.log_keep_files, 3);
        assert!(outcome.corrupt.unwrap().contains("logMaxMiB"));
    }

    #[test]
    fn explicit_false_for_confirm_before_install_survives_roundtrip() {
        let paths = DataPaths::under_root(&temp_root("settings-explicit-false"));
        paths.ensure_dirs().unwrap();
        let settings = Settings {
            confirm_before_install: false,
            ..Settings::default()
        };
        save(&paths.settings_file(), &settings).unwrap();
        let outcome = load_or_init(&paths.settings_file()).unwrap();
        assert_eq!(outcome.corrupt, None);
        assert_eq!(outcome.settings.confirm_before_install, false);
    }

    #[test]
    fn save_then_load_is_consistent() {
        let root = temp_root("settings-roundtrip");
        let paths = DataPaths::under_root(&root);
        paths.ensure_dirs().unwrap();
        let settings = Settings {
            auto_channel: Some("alpha".to_owned()),
            auto_install: true,
            confirm_before_install: false,
            launch_args: vec![
                "--no-open".to_owned(),
                "--port".to_owned(),
                "3123".to_owned(),
            ],
            work_dir: root.display().to_string(),
            auto_open_browser: false,
            auto_scroll: false,
            output_buffer_lines: 1,
            log_max_mib: 1024,
            log_keep_files: 50,
            oplog_max_mib: 1,
            oplog_keep_files: 1,
            stop_instance_on_exit: false,
            autostart_mode: AutostartMode::Window,
        };
        save(&paths.settings_file(), &settings).unwrap();
        assert_eq!(
            load_or_init(&paths.settings_file()).unwrap().settings,
            settings
        );
    }

    #[test]
    fn patch_changes_only_given_fields() {
        let mut settings = Settings {
            auto_channel: Some("next".to_owned()),
            output_buffer_lines: 100,
            ..Settings::default()
        };
        let problems = apply_patch(&mut settings, &serde_json::json!({ "autoInstall": true }));
        assert!(problems.is_empty(), "实际：{problems:?}");
        assert_eq!(settings.auto_install, true);
        assert_eq!(settings.auto_channel, Some("next".to_owned()));
        assert_eq!(settings.output_buffer_lines, 100);
        assert_eq!(settings.confirm_before_install, true);
    }

    #[test]
    fn patch_out_of_range_falls_back_to_default() {
        let mut settings = Settings {
            output_buffer_lines: 100,
            ..Settings::default()
        };
        let problems = apply_patch(
            &mut settings,
            &serde_json::json!({ "outputBufferLines": 0 }),
        );
        assert_eq!(settings.output_buffer_lines, 5000);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("outputBufferLines"));
    }

    #[test]
    fn patch_type_mismatch_keeps_previous_value() {
        let mut settings = Settings {
            log_max_mib: 9,
            ..Settings::default()
        };
        let problems = apply_patch(&mut settings, &serde_json::json!({ "logMaxMiB": "big" }));
        assert_eq!(settings.log_max_mib, 9, "类型不符时保持原值");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn patch_null_clears_auto_channel() {
        let mut settings = Settings {
            auto_channel: Some("latest".to_owned()),
            ..Settings::default()
        };
        let problems = apply_patch(&mut settings, &serde_json::json!({ "autoChannel": null }));
        assert!(problems.is_empty());
        assert_eq!(settings.auto_channel, None);
    }

    #[test]
    fn boundary_values_are_accepted() {
        let mut settings = Settings::default();
        let problems = apply_patch(
            &mut settings,
            &serde_json::json!({
                "outputBufferLines": 100000,
                "logMaxMiB": 1024,
                "logKeepFiles": 1,
                "oplogMaxMiB": 1,
                "oplogKeepFiles": 50,
            }),
        );
        assert!(problems.is_empty(), "实际：{problems:?}");
        assert_eq!(settings.output_buffer_lines, 100000);
        assert_eq!(settings.log_max_mib, 1024);
        assert_eq!(settings.log_keep_files, 1);
        assert_eq!(settings.oplog_keep_files, 50);
    }

    #[test]
    fn work_dir_empty_falls_back_to_default() {
        let mut settings = Settings::default();
        let problems = apply_patch(&mut settings, &serde_json::json!({ "workDir": "   " }));
        assert_eq!(settings.work_dir, default_work_dir());
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn unknown_keys_are_ignored_on_both_paths() {
        let mut settings = Settings {
            auto_scroll: false,
            ..Settings::default()
        };
        let problems = apply_patch(&mut settings, &serde_json::json!({ "futureOption": 1 }));
        assert!(problems.is_empty());
        assert_eq!(settings.auto_scroll, false);
        let parsed = parse("{\"futureOption\": 1, \"autoScroll\": false}");
        assert_eq!(parsed.corrupt, None);
        assert_eq!(parsed.settings.auto_scroll, false);
    }

    #[test]
    fn settings_store_is_the_frozen_entry_point() {
        let paths = DataPaths::under_root(&temp_root("settings-store"));
        paths.ensure_dirs().unwrap();
        let store = SettingsStore::load_or_init(paths.settings_file()).unwrap();
        assert_eq!(store.startup_problem(), None);
        assert_eq!(store.file(), paths.settings_file());
        assert_eq!(store.snapshot(), Settings::default());

        let (next, problems) = store
            .apply_patch(&serde_json::json!({ "autoChannel": "next" }))
            .unwrap();
        assert!(problems.is_empty(), "实际：{problems:?}");
        assert_eq!(next.auto_channel, Some("next".to_owned()));
        assert_eq!(store.snapshot().auto_channel, Some("next".to_owned()));
        // 补丁即时持久化：重新从磁盘读也应当看到新值
        assert_eq!(
            load_or_init(&paths.settings_file())
                .unwrap()
                .settings
                .auto_channel,
            Some("next".to_owned())
        );
    }

    #[test]
    fn settings_store_reports_the_startup_problem() {
        let paths = DataPaths::under_root(&temp_root("settings-store-corrupt"));
        paths.ensure_dirs().unwrap();
        fs::write(paths.settings_file(), "{ 坏").unwrap();
        let store = SettingsStore::load_or_init(paths.settings_file()).unwrap();
        let problem = store.startup_problem().expect("应保留启动时的回落原因");
        assert!(problem.contains("JSON 解析失败"), "实际：{problem}");
        assert_eq!(store.snapshot(), Settings::default());
    }

    fn policy() -> crate::m0::logging::RollPolicy {
        crate::m0::logging::RollPolicy {
            max_bytes: 1024 * 1024,
            keep_files: 3,
        }
    }

    #[test]
    fn corrupt_load_is_recorded_in_oplog() {
        let paths = DataPaths::under_root(&temp_root("settings-corrupt-oplog"));
        paths.ensure_dirs().unwrap();
        fs::write(paths.settings_file(), "{ 这不是 JSON").unwrap();

        let outcome = load_or_init(&paths.settings_file()).unwrap();
        let raw = outcome.corrupt.clone().expect("应记录回落原因");
        let oplog = Oplog::new(paths.oplog_file());
        let entry = record_corrupt(&oplog, &paths.settings_file(), &raw, policy()).unwrap();
        assert_eq!(entry.code.as_deref(), Some(error::SETTINGS_CORRUPT));
        assert_eq!(entry.result, crate::m0::oplog::OplogResult::Fail);

        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].code.as_deref(), Some("settings.corrupt"));
        assert!(
            page.entries[0]
                .raw
                .as_deref()
                .unwrap()
                .contains("JSON 解析失败")
        );
        // 损坏文件不被覆盖：保留现场，不自动「修复」（NFR-7）
        assert_eq!(
            fs::read_to_string(paths.settings_file()).unwrap(),
            "{ 这不是 JSON"
        );
    }

    #[test]
    fn settings_change_is_recorded_with_before_and_after() {
        let paths = DataPaths::under_root(&temp_root("settings-change-oplog"));
        paths.ensure_dirs().unwrap();
        let before = Settings::default();
        let mut after = before.clone();
        apply_patch(&mut after, &serde_json::json!({ "autoChannel": "alpha" }));

        let oplog = Oplog::new(paths.oplog_file());
        let entry =
            record_change(&oplog, &paths.settings_file(), &before, &after, policy()).unwrap();
        assert_eq!(entry.action, "settings.update");
        assert_eq!(entry.result, crate::m0::oplog::OplogResult::Ok);
        let raw = entry.raw.expect("应含前后值");
        assert!(raw.contains("\"alpha\""), "实际：{raw}");
        assert!(raw.contains("→"));
    }
}

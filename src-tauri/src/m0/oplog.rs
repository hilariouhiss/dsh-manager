//! 操作记录：JSONL 追加、按大小滚动、分页读取（主计划 §3.6、NFR-6）。
//!
//! 追加写入不重写整文件，崩溃最多丢最后一行（设计文档 §7.2 的取舍）。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{self, AppError, CmdResult};
use crate::m0::logging::{self, RollPolicy};

/// 一条操作记录的结果（主计划 §3.6："ok" | "fail"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OplogResult {
    Ok,
    Fail,
}

/// 操作记录实体（主计划 §3.6，字段名 camelCase）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OplogEntry {
    pub timestamp_ms: u64,
    pub action: String,
    pub target: String,
    pub result: OplogResult,
    pub code: Option<String>,
    pub raw: Option<String>,
}

impl OplogEntry {
    pub fn ok(action: &str, target: &str) -> Self {
        Self {
            timestamp_ms: crate::m0::now_ms(),
            action: action.to_owned(),
            target: target.to_owned(),
            result: OplogResult::Ok,
            code: None,
            raw: None,
        }
    }

    pub fn fail(action: &str, target: &str, code: &str, raw: impl Into<String>) -> Self {
        Self {
            timestamp_ms: crate::m0::now_ms(),
            action: action.to_owned(),
            target: target.to_owned(),
            result: OplogResult::Fail,
            code: Some(code.to_owned()),
            raw: Some(raw.into()),
        }
    }
}

/// 分页结果（主计划 §3.4 命令 16 的出参）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OplogPage {
    pub entries: Vec<OplogEntry>,
    pub total: usize,
}

/// 操作记录的唯一入口（主计划 §3.3.2 冻结了 `AppState.oplog: Arc<Oplog>`）。
#[derive(Debug, Clone)]
pub struct Oplog {
    file: PathBuf,
}

impl Oplog {
    pub fn new(file: PathBuf) -> Self {
        Self { file }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// 追加一条（调用方负责随后推 oplog:entry 事件）。
    pub fn append(&self, entry: &OplogEntry, policy: RollPolicy) -> CmdResult<()> {
        append(&self.file, entry, policy)
    }

    /// 分页读取，最新一条在前；解析不了的行跳过，不影响其它行。
    pub fn read_page(&self, offset: usize, limit: usize) -> CmdResult<OplogPage> {
        read_page(&self.file, offset, limit)
    }
}

/// 追加一条到指定文件（调用方负责随后推 oplog:entry 事件）。
pub fn append(file: &Path, entry: &OplogEntry, policy: RollPolicy) -> CmdResult<()> {
    let line = serde_json::to_string(entry).expect("OplogEntry 只含字符串与数字，序列化不会失败");
    logging::append_line(file, &line, policy)
}

/// 分页读取，**最新一条在前**；解析不了的行（崩溃时的半行）跳过，不影响其它行。
pub fn read_page(path: &Path, offset: usize, limit: usize) -> CmdResult<OplogPage> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(AppError::new(
                error::PATH_PERMISSION_DENIED,
                format!("无法读取操作记录：{}", path.display()),
                "确认该文件未被其他程序占用且当前用户有读取权限，然后重试。",
            )
            .with_raw(e.to_string()));
        }
    };
    let mut entries: Vec<OplogEntry> = text
        .lines()
        .filter_map(|line| serde_json::from_str::<OplogEntry>(line).ok())
        .collect();
    let total = entries.len();
    entries.reverse();
    Ok(OplogPage {
        entries: entries.into_iter().skip(offset).take(limit).collect(),
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m0::logging::rolled_path;
    use crate::m0::paths::{DataPaths, temp_root};

    fn fixture(tag: &str) -> DataPaths {
        let paths = DataPaths::under_root(&temp_root(tag));
        paths.ensure_dirs().unwrap();
        paths
    }

    fn pol() -> RollPolicy {
        RollPolicy {
            max_bytes: 1024 * 1024,
            keep_files: 3,
        }
    }

    #[test]
    fn appends_one_parseable_json_per_line() {
        let paths = fixture("oplog-jsonl");
        let first = OplogEntry::ok("settings.update", "settings.json");
        let second = OplogEntry::fail(
            "settings.load",
            "settings.json",
            error::SETTINGS_CORRUPT,
            "JSON 解析失败",
        );
        append(&paths.oplog_file(), &first, pol()).unwrap();
        append(&paths.oplog_file(), &second, pol()).unwrap();

        let text = fs::read_to_string(paths.oplog_file()).unwrap();
        assert_eq!(text.lines().count(), 2, "一行一条");
        let parsed: Vec<OplogEntry> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(parsed, vec![first, second]);
        assert!(parsed[0].timestamp_ms > 0);
        assert_eq!(parsed[1].result, OplogResult::Fail);
    }

    #[test]
    fn entries_serialize_with_camel_case_keys() {
        let entry = OplogEntry::ok("settings.update", "settings.json");
        let value = serde_json::to_value(&entry).unwrap();
        let object = value.as_object().unwrap();
        for key in ["timestampMs", "action", "target", "result", "code", "raw"] {
            assert!(object.contains_key(key), "缺字段 {key}");
        }
        assert_eq!(object["result"], serde_json::json!("ok"));
    }

    #[test]
    fn reads_newest_first_with_paging() {
        let paths = fixture("oplog-page");
        for action in ["a", "b", "c"] {
            append(&paths.oplog_file(), &OplogEntry::ok(action, "t"), pol()).unwrap();
        }
        let first_page = read_page(&paths.oplog_file(), 0, 2).unwrap();
        assert_eq!(first_page.total, 3);
        let actions: Vec<&str> = first_page
            .entries
            .iter()
            .map(|entry| entry.action.as_str())
            .collect();
        assert_eq!(actions, vec!["c", "b"]);

        let second_page = read_page(&paths.oplog_file(), 2, 2).unwrap();
        assert_eq!(second_page.total, 3);
        assert_eq!(second_page.entries.len(), 1);
        assert_eq!(second_page.entries[0].action, "a");
    }

    #[test]
    fn missing_file_yields_empty_page_not_error() {
        let paths = DataPaths::under_root(&temp_root("oplog-missing"));
        let page = read_page(&paths.oplog_file(), 0, 10).unwrap();
        assert_eq!(page.total, 0);
        assert!(page.entries.is_empty());
    }

    #[test]
    fn truncated_last_line_does_not_break_earlier_lines() {
        let paths = fixture("oplog-crash");
        append(&paths.oplog_file(), &OplogEntry::ok("first", "t"), pol()).unwrap();
        append(&paths.oplog_file(), &OplogEntry::ok("second", "t"), pol()).unwrap();
        let mut text = fs::read_to_string(paths.oplog_file()).unwrap();
        text.push_str("{\"timestampMs\":123,\"action\":\"tor");
        fs::write(paths.oplog_file(), text).unwrap();

        let page = read_page(&paths.oplog_file(), 0, 10).unwrap();
        assert_eq!(page.total, 2, "半行被跳过，已写入的两行仍可读");
        assert_eq!(page.entries[0].action, "second");
    }

    #[test]
    fn oplog_handle_is_the_frozen_entry_point() {
        let paths = fixture("oplog-handle");
        let oplog = Oplog::new(paths.oplog_file());
        oplog
            .append(&OplogEntry::ok("settings.update", "settings.json"), pol())
            .unwrap();
        assert_eq!(oplog.file(), paths.oplog_file());
        let page = oplog.read_page(0, 10).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.entries[0].action, "settings.update");
    }

    #[test]
    fn rolling_keeps_the_total_bounded() {
        let paths = fixture("oplog-roll");
        let policy = RollPolicy {
            max_bytes: 200,
            keep_files: 2,
        };
        for _ in 0..50 {
            append(
                &paths.oplog_file(),
                &OplogEntry::ok("settings.update", "settings.json"),
                policy,
            )
            .unwrap();
        }
        assert!(rolled_path(&paths.oplog_file(), 1).exists());
        assert!(!rolled_path(&paths.oplog_file(), 3).exists());
    }
}

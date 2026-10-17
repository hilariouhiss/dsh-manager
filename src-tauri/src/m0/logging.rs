//! 追加式文本落盘与按大小滚动（主计划 §3.6、NFR-5）。
//!
//! 操作记录与实例运行日志共用这一份实现，避免两套滚动逻辑各自漂移。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{self, AppError, CmdResult};
use crate::m0::settings::Settings;

/// 单文件上限与保留份数，来自设置项（§3.7）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollPolicy {
    pub max_bytes: u64,
    pub keep_files: u32,
}

/// 实例日志策略：logMaxMiB / logKeepFiles。
pub fn log_policy(settings: &Settings) -> RollPolicy {
    RollPolicy {
        max_bytes: u64::from(settings.log_max_mib) * 1024 * 1024,
        keep_files: settings.log_keep_files,
    }
}

/// 操作记录策略：oplogMaxMiB / oplogKeepFiles。
pub fn oplog_policy(settings: &Settings) -> RollPolicy {
    RollPolicy {
        max_bytes: u64::from(settings.oplog_max_mib) * 1024 * 1024,
        keep_files: settings.oplog_keep_files,
    }
}

/// 滚动后的文件名：<file>.1 是最近一次滚动的结果。
pub fn rolled_path(file: &Path, index: u32) -> PathBuf {
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{index}"));
    file.with_file_name(name)
}

/// M3 写实例日志的唯一入口（主计划 §3.3.2 冻结签名，T-I2 消费）。
#[derive(Debug, Clone)]
pub struct LogWriter {
    path: PathBuf,
    policy: RollPolicy,
}

impl LogWriter {
    /// 打开（或按 "<base>.1"、"<base>.2" 滚动后打开）某实例的日志文件。
    pub fn open(path: PathBuf, max_bytes: u64, keep_files: u32) -> CmdResult<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error("创建日志目录", parent, e))?;
        }
        Ok(Self {
            path,
            policy: RollPolicy {
                max_bytes,
                keep_files,
            },
        })
    }

    /// 追加一行（含换行）；超限时先滚动再写。
    pub fn append_line(&mut self, line: &str) -> CmdResult<()> {
        append_line(&self.path, line, self.policy)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 滚动原语：追加一行（自动补换行）。写入前若会超过上限，先滚动，避免单文件无限增长。
pub fn append_line(file: &Path, line: &str, policy: RollPolicy) -> CmdResult<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|e| io_error("创建日志目录", parent, e))?;
    }
    let current = fs::metadata(file).map(|meta| meta.len()).unwrap_or(0);
    let incoming = line.len() as u64 + 1;
    if current > 0 && current + incoming > policy.max_bytes {
        roll(file, policy.keep_files)?;
    }
    let mut handle = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| io_error("打开日志文件", file, e))?;
    writeln!(handle, "{line}").map_err(|e| io_error("写入日志文件", file, e))
}

/// 把 <file> → <file>.1 → <file>.2 …；超出 keep_files 的最旧一份被删除。
pub fn roll(file: &Path, keep_files: u32) -> CmdResult<()> {
    if !file.exists() {
        return Ok(());
    }
    if keep_files == 0 {
        return fs::remove_file(file).map_err(|e| io_error("删除日志文件", file, e));
    }
    let oldest = rolled_path(file, keep_files);
    if oldest.exists() {
        fs::remove_file(&oldest).map_err(|e| io_error("删除最旧日志", &oldest, e))?;
    }
    for index in (1..keep_files).rev() {
        let from = rolled_path(file, index);
        if from.exists() {
            let to = rolled_path(file, index + 1);
            fs::rename(&from, &to).map_err(|e| io_error("滚动日志", &from, e))?;
        }
    }
    let first = rolled_path(file, 1);
    fs::rename(file, &first).map_err(|e| io_error("滚动日志", file, e))
}

fn io_error(action: &str, path: &Path, e: std::io::Error) -> AppError {
    AppError::new(
        error::PATH_PERMISSION_DENIED,
        format!("{action}失败：{}", path.display()),
        "确认该目录存在且当前用户有写权限，然后重试。",
    )
    .with_raw(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::m0::paths::temp_root;
    use crate::m0::settings::Settings;

    fn pol(max_bytes: u64, keep_files: u32) -> RollPolicy {
        RollPolicy {
            max_bytes,
            keep_files,
        }
    }

    /// 主计划 §3.3.2 冻结的入口：LogWriter::open + append_line
    fn writer(dir: &Path, name: &str, max_bytes: u64, keep_files: u32) -> LogWriter {
        LogWriter::open(dir.join(name), max_bytes, keep_files).unwrap()
    }

    #[test]
    fn append_line_writes_one_line_per_call() {
        let dir = temp_root("logging-append");
        let mut writer = writer(&dir, "instance.log", 1024, 3);
        writer.append_line("第一行").unwrap();
        writer.append_line("第二行").unwrap();
        let text = fs::read_to_string(writer.path()).unwrap();
        assert_eq!(text, "第一行\n第二行\n");
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn open_creates_missing_directories() {
        let mut writer = LogWriter::open(
            temp_root("logging-mkdir")
                .join("logs")
                .join("instance-x.log"),
            1024,
            3,
        )
        .unwrap();
        writer.append_line("hi").unwrap();
        assert!(writer.path().exists());
    }

    #[test]
    fn rolling_happens_before_exceeding_the_limit() {
        let dir = temp_root("logging-roll");
        let mut writer = writer(&dir, "instance.log", 64, 3);
        for _ in 0..20 {
            writer.append_line("0123456789").unwrap();
        }
        let file = dir.join("instance.log");
        assert!(rolled_path(&file, 1).exists(), "超过上限后应产生 .1");
        assert!(
            !rolled_path(&file, 4).exists(),
            "保留份数不得超过 keep_files"
        );
        let current = fs::metadata(&file).unwrap().len();
        assert!(current <= 64, "当前文件大小应受控：{current}");
    }

    #[test]
    fn keep_files_is_respected() {
        let dir = temp_root("logging-keep");
        let mut writer = writer(&dir, "instance.log", 32, 1);
        for _ in 0..10 {
            writer.append_line("0123456789").unwrap();
        }
        let file = dir.join("instance.log");
        assert!(rolled_path(&file, 1).exists());
        assert!(
            !rolled_path(&file, 2).exists(),
            "keep_files = 1 时只保留一份"
        );
    }

    #[test]
    fn policies_come_from_settings() {
        let settings = Settings {
            log_max_mib: 5,
            log_keep_files: 3,
            oplog_max_mib: 2,
            oplog_keep_files: 4,
            ..Settings::default()
        };
        assert_eq!(log_policy(&settings), pol(5 * 1024 * 1024, 3));
        assert_eq!(oplog_policy(&settings), pol(2 * 1024 * 1024, 4));
    }
}

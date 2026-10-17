//! 测试用 RegistrySource 假实现：注入「已发布 / 未发布 / 失败」三种响应。
//!
//! 本文件不参与正常构建，只由测试模块通过 #[path] 引入。
//!
//! 按端口全貌提供注入能力；当前任务只用到探测，下载注入供 T-V5 使用。
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use crate::error::{AppError, CmdResult};
use crate::m0::ProgressSink;
use crate::ports::registry::{ChannelProbe, PackageRelease, RegistrySource};

/// 可注入的探测结果。
#[derive(Clone)]
pub enum FakeProbe {
    Published(PackageRelease),
    Unpublished,
    Failed(String, String),
}

/// 注入的下载行为。
///
/// `Loop` 按 chunk 分块写盘并逐块 `progress.report`，用于驱动「取消」「百分比单调」两条用例；
/// `Bytes` 一次性写盘，用于普通成功路径。
#[derive(Clone)]
pub enum FakeDownload {
    Bytes(Vec<u8>),
    Loop { size: u64, chunk: u64 },
}

/// 下载结果：成功行为，或（错误码, 原因, 下一步）。
pub type FakeDownloadResult = Result<FakeDownload, (String, String, String)>;

pub struct FakeRegistry {
    probes: HashMap<String, Vec<FakeProbe>>,
    /// 每条通道各自的响应下标：用尽后重复最后一个（便于「先成功、后失败」的缓存用例）。
    /// 不能用一个全局计数：三条通道并发探测时，别的通道会推进本通道的序列。
    probe_offsets: Mutex<HashMap<String, usize>>,
    downloads: HashMap<String, FakeDownloadResult>,
    probe_calls: AtomicUsize,
    download_calls: AtomicUsize,
    log: Mutex<Vec<String>>,
}

impl FakeRegistry {
    pub fn new() -> Self {
        Self {
            probes: HashMap::new(),
            probe_offsets: Mutex::new(HashMap::new()),
            downloads: HashMap::new(),
            probe_calls: AtomicUsize::new(0),
            download_calls: AtomicUsize::new(0),
            log: Mutex::new(Vec::new()),
        }
    }

    /// 为某通道按顺序注入响应；用尽后重复最后一个。
    pub fn with_probe(mut self, channel: &str, responses: Vec<FakeProbe>) -> Self {
        assert!(!responses.is_empty(), "至少注入一个响应");
        self.probes.insert(channel.to_string(), responses);
        self
    }

    /// 为某个 tarball URL 注入下载结果。
    pub fn with_download(mut self, url: &str, result: FakeDownloadResult) -> Self {
        self.downloads.insert(url.to_string(), result);
        self
    }

    pub fn probe_calls(&self) -> usize {
        self.probe_calls.load(Ordering::SeqCst)
    }

    pub fn download_calls(&self) -> usize {
        self.download_calls.load(Ordering::SeqCst)
    }

    /// 调用流水，形如 ["probe:latest", "probe:alpha"]，供并发顺序断言。
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// 下次探测固定返回该结果（覆盖原有注入）。
    pub fn set_probe(&mut self, channel: &str, response: FakeProbe) {
        self.probes.insert(channel.to_string(), vec![response]);
    }

    fn snapshot(r: &PackageRelease) -> PackageRelease {
        PackageRelease {
            version: r.version.clone(),
            published_at_iso: r.published_at_iso.clone(),
            integrity: r.integrity.clone(),
            tarball_url: r.tarball_url.clone(),
            unpacked_size: r.unpacked_size,
        }
    }
}

impl Default for FakeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn to_probe(f: &FakeProbe) -> ChannelProbe {
    match f {
        FakeProbe::Published(r) => ChannelProbe::Published(FakeRegistry::snapshot(r)),
        FakeProbe::Unpublished => ChannelProbe::Unpublished,
        FakeProbe::Failed(code, reason) => ChannelProbe::Failed(
            AppError::new(
                code.as_str(),
                reason.clone(),
                "检查网络连接或系统代理设置后重试。",
            )
            .retryable(true),
        ),
    }
}

#[async_trait]
impl RegistrySource for FakeRegistry {
    async fn probe(&self, channel: &str) -> ChannelProbe {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.log.lock().unwrap().push(format!("probe:{channel}"));
        let Some(list) = self.probes.get(channel) else {
            return ChannelProbe::Failed(
                AppError::new(
                    crate::error::REGISTRY_PARSE,
                    format!("测试夹具没有为通道 {channel} 注入响应。"),
                    "请修正测试夹具。",
                )
                .retryable(false),
            );
        };
        let idx = {
            let mut offsets = self.probe_offsets.lock().unwrap();
            let offset = offsets.entry(channel.to_string()).or_insert(0);
            let idx = *offset;
            *offset += 1;
            idx
        };
        let pick = list.get(idx).unwrap_or_else(|| list.last().unwrap());
        to_probe(pick)
    }

    /// 按块写盘，每块之后 `progress.report(downloaded, total)`——与真实适配器同一形状，
    /// 使 T-V5 的「取消后删文件」「百分比单调不减且终止于 100」可以在假实现上验证。
    async fn download(
        &self,
        release: &PackageRelease,
        dest: &Path,
        cancel: crate::m0::cancel::CancelToken,
        progress: ProgressSink,
    ) -> CmdResult<()> {
        self.download_calls.fetch_add(1, Ordering::SeqCst);
        self.log
            .lock()
            .unwrap()
            .push(format!("download:{}", release.tarball_url));
        let Some(injected) = self.downloads.get(&release.tarball_url) else {
            return Err(AppError::new(
                crate::error::REGISTRY_PARSE,
                format!("测试夹具没有为 {} 注入下载结果。", release.tarball_url),
                "请修正测试夹具。",
            )
            .retryable(false));
        };
        let bytes: Vec<u8> = match injected {
            Ok(FakeDownload::Bytes(b)) => b.clone(),
            Ok(FakeDownload::Loop { size, chunk }) => {
                let mut v = Vec::with_capacity(*size as usize);
                let pattern = b"0123456789";
                while (v.len() as u64) < *size {
                    v.extend_from_slice(pattern);
                }
                v.truncate(*size as usize);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        AppError::new(
                            crate::error::PATH_PERMISSION_DENIED,
                            format!("测试夹具建目录失败：{e}"),
                            "检查临时目录权限后重试。",
                        )
                    })?;
                }
                let mut file = std::fs::File::create(dest).map_err(|e| {
                    AppError::new(
                        crate::error::PATH_PERMISSION_DENIED,
                        format!("测试夹具建文件失败：{e}"),
                        "检查临时目录权限后重试。",
                    )
                })?;
                use std::io::Write as _;
                let mut done: u64 = 0;
                let total = *size;
                progress.report(0, Some(total));
                while done < total {
                    if cancel.is_cancelled() {
                        drop(file);
                        let _ = std::fs::remove_file(dest);
                        return Err(AppError::new(
                            crate::error::DOWNLOAD_CANCELLED,
                            "下载已被取消。",
                            "需要时重新点击「安装」即可。",
                        )
                        .retryable(true));
                    }
                    let end = (done + *chunk).min(total);
                    file.write_all(&v[done as usize..end as usize])
                        .map_err(|e| {
                            AppError::new(
                                crate::error::PATH_PERMISSION_DENIED,
                                format!("测试夹具写文件失败：{e}"),
                                "检查临时目录权限后重试。",
                            )
                        })?;
                    done = end;
                    progress.report(done, Some(total));
                }
                file.flush().ok();
                return Ok(());
            }
            Err((code, reason, next)) => {
                return Err(
                    AppError::new(code.as_str(), reason.clone(), next.clone()).retryable(true)
                );
            }
        };
        if cancel.is_cancelled() {
            return Err(AppError::new(
                crate::error::DOWNLOAD_CANCELLED,
                "下载已被取消。",
                "需要时重新点击「安装」即可。",
            )
            .retryable(true));
        }
        // Bytes 也要真写盘：T-V5 的 download_and_verify 会在下载后读取并校验它
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::new(
                    crate::error::PATH_PERMISSION_DENIED,
                    format!("测试夹具建目录失败：{e}"),
                    "检查临时目录权限后重试。",
                )
            })?;
        }
        let total = bytes.len() as u64;
        progress.report(0, Some(total));
        std::fs::write(dest, &bytes).map_err(|e| {
            AppError::new(
                crate::error::PATH_PERMISSION_DENIED,
                format!("测试夹具写文件失败：{e}"),
                "检查临时目录权限后重试。",
            )
        })?;
        progress.report(total, Some(total));
        Ok(())
    }
}

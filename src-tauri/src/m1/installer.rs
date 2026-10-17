//! 安装包获取：空间检查 → 下载 → 进度 → 取消 → SRI 校验（主计划 T-V5，顺序不可颠倒）。

use sha2::Digest as _;

use crate::error::{AppError, CmdResult};

/// SRI 支持的两种摘要算法。**两种都实现，不硬编码单一算法**（§3.8 D-2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SriAlgorithm {
    Sha512,
    Sha256,
}

impl SriAlgorithm {
    pub fn as_str(&self) -> &'static str {
        match self {
            SriAlgorithm::Sha512 => "sha512",
            SriAlgorithm::Sha256 => "sha256",
        }
    }
}

/// 解析后的 SRI：算法 + **已解码的字节**（比较必须按字节，不做字符串比较）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SriDigest {
    pub algorithm: SriAlgorithm,
    pub expected: Vec<u8>,
}

fn integrity_error(reason: String) -> AppError {
    AppError::new(
        crate::error::DOWNLOAD_INTEGRITY_MISMATCH,
        reason,
        "删除已下载的安装包后重试；若持续失败，请检查网络与镜像配置。",
    )
    .retryable(true)
}

fn no_integrity_error() -> AppError {
    AppError::new(
        crate::error::DOWNLOAD_NO_INTEGRITY,
        "注册表没有为这个版本提供完整性摘要，无法确认安装包未被篡改。",
        "换一个通道或版本重试；若持续缺失，请到 npm 页面确认该版本是否完整发布。",
    )
}

/// 下载前的硬检查：注册表没有 integrity 就**拒绝安装**（§3.8 D-2、FR-1.5）。
///
/// 它与 `verify_integrity` 共用同一份错误构造：两条路径必须给出同一个错误码，
/// 否则「缺摘要」会被误判成「摘要不匹配」（§3.2 的两个码语义不同）。
pub fn require_integrity(integrity: Option<&str>) -> CmdResult<()> {
    match integrity {
        Some(_) => Ok(()),
        None => Err(no_integrity_error()),
    }
}

/// 标准 base64 解码（含 / 不含填充都接受）。
///
/// 仓库基线（主计划 §1.2）没有 base64 依赖，而 SRI 必须**按字节**比较（§3.8 D-2），
/// 因此这里只实现解码所需的最小部分：非法字符或不可能的余数返回 None。
fn decode_base64(payload: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let trimmed = payload.trim().trim_end_matches('=');
    // 单个 base64 字符不可能组成一个字节；空载荷同样是非法 SRI。
    if trimmed.is_empty() || trimmed.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for &c in trimmed.as_bytes() {
        accumulator = (accumulator << 6) | sextet(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// 按 SRI 前缀选择算法并解码 base64。无前缀、未知前缀、非法 base64 一律拒绝。
pub fn parse_sri(integrity: &str) -> CmdResult<SriDigest> {
    let Some((prefix, payload)) = integrity.split_once('-') else {
        return Err(integrity_error(format!(
            "注册表返回的摘要格式无法识别（缺少 sha512-/sha256- 前缀）：{integrity}。"
        )));
    };
    let algorithm = match prefix {
        "sha512" => SriAlgorithm::Sha512,
        "sha256" => SriAlgorithm::Sha256,
        other => {
            return Err(integrity_error(format!(
                "注册表返回的摘要算法不受支持：{other}（只支持 sha512 与 sha256）。"
            )));
        }
    };
    let expected = decode_base64(payload).ok_or_else(|| {
        integrity_error(format!("注册表返回的摘要不是合法的 base64：{payload}。"))
    })?;
    Ok(SriDigest {
        algorithm,
        expected,
    })
}

/// 校验字节流是否与 SRI 一致。`integrity` 为 None 时**拒绝安装**（§3.8 D-2）。
pub fn verify_integrity(integrity: Option<&str>, bytes: &[u8]) -> CmdResult<()> {
    require_integrity(integrity)?;
    let digest = parse_sri(integrity.unwrap_or_default())?;
    let actual: Vec<u8> = match digest.algorithm {
        SriAlgorithm::Sha512 => sha2::Sha512::digest(bytes).to_vec(),
        SriAlgorithm::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
    };
    if actual != digest.expected {
        return Err(integrity_error(format!(
            "安装包的 {} 摘要与注册表不一致：期望 {} 字节，实际 {} 字节。",
            digest.algorithm.as_str(),
            digest.expected.len(),
            actual.len()
        )));
    }
    Ok(())
}

/// 下载阶段的进度回调节点（§3.5 事件 task:progress 的载荷来源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPhase {
    Checking,
    Downloading,
    Verifying,
    Installing,
}

impl InstallPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallPhase::Checking => "checking",
            InstallPhase::Downloading => "downloading",
            InstallPhase::Verifying => "verifying",
            InstallPhase::Installing => "installing",
        }
    }
}

/// 一次进度事件。percent 为整数百分比，remainingMs 无法估算时为 None。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskProgress {
    pub percent: u32,
    pub remaining_ms: Option<u64>,
    pub phase: InstallPhase,
}

/// 进度计算：**百分比单调不减，且必然终止于 100**。
///
/// elapsed 由调用方传入，便于测试用构造的时间推进验证单调性。
pub fn progress_from(
    total: u64,
    done: u64,
    _elapsed: std::time::Duration,
    last_percent: u32,
) -> u32 {
    if total == 0 {
        return 100;
    }
    let raw = (done.min(total) as u128 * 100 / total as u128) as u32;
    raw.max(last_percent)
}

/// 按已下载速率估算剩余毫秒；速率未知或已完成时返回 None（FR-1.5「展示剩余时间」）。
pub fn estimate_remaining_ms(total: u64, done: u64, elapsed: std::time::Duration) -> Option<u64> {
    if done == 0 || done >= total {
        return None;
    }
    let elapsed_ms = elapsed.as_millis() as u64;
    if elapsed_ms == 0 {
        return None;
    }
    let rate = done as f64 / elapsed_ms as f64; // 字节/毫秒
    if rate <= 0.0 {
        return None;
    }
    Some(((total - done) as f64 / rate) as u64)
}

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::m0::ProgressSink;
use crate::m0::cancel::CancelToken;
use crate::ports::registry::{PackageRelease, RegistrySource};

/// 一次安装取包的输入。
pub struct InstallRequest {
    pub channel: String,
    pub release: PackageRelease,
    /// 目标文件（§3.6：`%LOCALAPPDATA%\Temp\dsh-manager\<pkg>-<version>.tgz`）
    pub dest: PathBuf,
    /// **`dest` 所在卷**的可用空间（调用方用 `available_space_for(&dest)` 读出，测试注入固定值）。
    /// 必须查「真正要写入的那个目录」所在的卷——TEMP 常被指到别的盘（审计重要-10）。
    pub available_bytes: u64,
    /// 压缩包字节数（来自 Content-Length；未知为 None）
    pub tarball_bytes: Option<u64>,
}

impl InstallRequest {
    /// 安装前所需空间：解包体积 + 压缩包本身（FR-1.5）。
    pub fn space_required(&self) -> u64 {
        self.release.space_required(self.tarball_bytes)
    }
}

/// 下载并校验通过后的结果。
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha_algorithm: SriAlgorithm,
}

// **不在这里拼临时包路径**：§3.6 冻结由 DataPaths::tarball(package, version) 净化并产出
// @deepseek-ai-dsh-<version>.tgz（作用域包名里的 / 必须替换，否则会变成子目录）。
// dest 由调用方（start_install）传入，本模块只负责「往这个路径下载」。

/// 用 `sysinfo` 的磁盘能力读出包含 `path` 的挂载点可用空间（§1.2：sysinfo 只开 system + disk）。
///
/// 找不到包含该路径的挂载点时返回 0：宁可判为空间不足，也不拿别的卷的余量冒险。
pub fn available_space_for(path: &Path) -> u64 {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut best: Option<(usize, u64)> = None;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if path.starts_with(mount) {
            let len = mount.as_os_str().len();
            match best {
                Some((best_len, _)) if best_len >= len => {}
                _ => best = Some((len, disk.available_space())),
            }
        }
    }
    best.map(|(_, free)| free).unwrap_or(0)
}

/// 步骤 1：空间检查与传输加密检查。**必须在任何下载动作之前调用**（FR-1.5、§3.8 D-2）。
pub fn check_space(req: &InstallRequest) -> CmdResult<()> {
    if !req.release.is_https() {
        return Err(AppError::new(
            crate::error::DOWNLOAD_INTEGRITY_MISMATCH,
            format!(
                "安装包地址不是 HTTPS，传输过程未加密：{}。",
                req.release.tarball_url
            ),
            "换一个通道或版本重试；若持续如此，请检查镜像配置。",
        )
        .retryable(true));
    }
    let required = req.space_required();
    if req.available_bytes < required {
        return Err(AppError::new(
            crate::error::DOWNLOAD_INSUFFICIENT_SPACE,
            format!(
                "磁盘空间不足：安装需要约 {} MB，目标盘可用 {} MB。",
                required / (1024 * 1024),
                req.available_bytes / (1024 * 1024)
            ),
            "清理磁盘空间后重试，或到设置里把临时目录所在盘换到空间充足的磁盘。",
        )
        .retryable(true));
    }
    Ok(())
}

pub(crate) fn cancelled_error() -> AppError {
    AppError::new(
        crate::error::DOWNLOAD_CANCELLED,
        "下载已被取消。",
        "需要时重新点击「安装」即可。",
    )
    .retryable(true)
}

/// 步骤 2~4：**经端口下载** → 换算进度 → 可取消 → SRI 校验。
///
/// 顺序不可颠倒（§3.3 + T-V5 要点）：空间与 HTTPS 检查 → `registry.download(..)` → 校验。
/// 校验不通过时**删除那个坏文件**（§3.6 消歧：已证明不可用，留着会诱导手工安装坏包）；
/// 用户取消时同样删除半成品（NFR-2）；校验通过但安装失败由 T-V6 保留并给手工命令。
///
/// **不得绕过端口直接 `m0::net::client()` 下载**——那会让端口退化为死代码，
/// 超时与取消也就无法用假实现注入（§3.3 契约、§7.4 #4）。
///
/// 进度经 `on_progress: TaskProgress` 交给调用方（M1 侧事件载荷）；端口那一侧收到的是
/// 原始字节数，换算在本函数内完成（§3.3.1 分层约束）。
pub async fn download_and_verify(
    registry: &dyn RegistrySource,
    req: &InstallRequest,
    cancel: CancelToken,
    on_progress: impl Fn(TaskProgress) + Send + Sync + 'static,
) -> CmdResult<InstallOutcome> {
    on_progress(TaskProgress {
        percent: 0,
        remaining_ms: None,
        phase: InstallPhase::Checking,
    });
    check_space(req)?;
    require_integrity(req.release.integrity.as_deref())?;
    if cancel.is_cancelled() {
        return Err(cancelled_error());
    }

    let total = req.tarball_bytes;
    let started = Instant::now();
    let last = Arc::new(AtomicU32::new(0));
    let on_progress = Arc::new(on_progress);
    on_progress.as_ref()(TaskProgress {
        percent: 0,
        remaining_ms: None,
        phase: InstallPhase::Downloading,
    });

    // 端口只给原始字节数；这里换算成百分比与剩余时间，并保证单调不减。
    let pump = {
        let last = Arc::clone(&last);
        let on_progress = Arc::clone(&on_progress);
        ProgressSink::new(move |downloaded: u64, reported_total: Option<u64>| {
            let total = reported_total.or(total).unwrap_or(0);
            let previous = last.load(Ordering::SeqCst);
            let converted = convert_progress(downloaded, Some(total), started.elapsed(), previous);
            if converted.percent != previous {
                last.store(converted.percent, Ordering::SeqCst);
                on_progress.as_ref()(converted);
            }
        })
    };
    if let Err(e) = registry
        .download(&req.release, &req.dest, cancel.clone(), pump)
        .await
    {
        // §3.6 消歧：用户取消后清理半成品（NFR-2）；其他下载失败保持端口原文。
        if e.code == crate::error::DOWNLOAD_CANCELLED {
            let _ = std::fs::remove_file(&req.dest);
        }
        return Err(e);
    }

    // 阶段推进到校验：此时下载已完成，进度封顶 100。
    on_progress.as_ref()(TaskProgress {
        percent: 100,
        remaining_ms: None,
        phase: InstallPhase::Verifying,
    });

    let bytes = std::fs::read(&req.dest).map_err(|e| {
        AppError::new(
            crate::error::PATH_PERMISSION_DENIED,
            format!("读取 {} 失败：{e}", req.dest.display()),
            "检查磁盘与目录权限后重试。",
        )
    })?;
    if let Err(e) = verify_integrity(req.release.integrity.as_deref(), &bytes) {
        // §3.6 消歧：校验不通过的文件已证明不可用，必须删除
        let _ = std::fs::remove_file(&req.dest);
        return Err(e);
    }
    let algorithm = parse_sri(req.release.integrity.as_deref().unwrap_or_default())?.algorithm;
    Ok(InstallOutcome {
        path: req.dest.clone(),
        bytes: bytes.len() as u64,
        sha_algorithm: algorithm,
    })
}

/// 把端口给的原始字节数换算成界面要的百分比与剩余时间。
///
/// 这是 §3.3.1「ProgressSink 只承载原始字节数」的 **M1 侧落点**：
/// `progress_from` 负责百分比（单调不减、终止于 100），
/// `estimate_remaining_ms` 负责剩余时间（按已下载速率估算）。
pub fn convert_progress(
    downloaded: u64,
    total: Option<u64>,
    elapsed: Duration,
    last_percent: u32,
) -> TaskProgress {
    let total = total.unwrap_or(0);
    TaskProgress {
        percent: progress_from(total, downloaded, elapsed, last_percent),
        remaining_ms: estimate_remaining_ms(total, downloaded, elapsed),
        phase: InstallPhase::Downloading,
    }
}

#[cfg(test)]
#[path = "../adapters/fake/fake_registry.rs"]
mod fake_registry;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_monotonic_and_ends_at_100() {
        let total = 1000u64;
        let mut last = 0u32;
        let mut seen = Vec::new();
        for (done, ms) in [
            (0u64, 0u64),
            (100, 10),
            (250, 25),
            (250, 30),
            (999, 90),
            (1000, 100),
        ] {
            let p = progress_from(total, done, std::time::Duration::from_millis(ms), last);
            assert!(p >= last, "百分比不得回退：{p} < {last}");
            last = p;
            seen.push(p);
        }
        assert_eq!(seen.last(), Some(&100), "必须终止于 100");
        assert_eq!(seen[0], 0);
    }

    #[test]
    fn zero_total_is_complete_not_a_division_panic() {
        assert_eq!(
            progress_from(0, 0, std::time::Duration::from_millis(1), 0),
            100
        );
    }

    #[test]
    fn remaining_time_uses_the_observed_rate() {
        // 100 字节用了 100 毫秒 → 速率 1 字节/毫秒；还剩 900 字节 → 约 900 毫秒
        let r = estimate_remaining_ms(1000, 100, std::time::Duration::from_millis(100)).unwrap();
        assert!((800..=1000).contains(&r), "剩余时间估算不合理：{r}");
        assert!(estimate_remaining_ms(1000, 0, std::time::Duration::from_millis(100)).is_none());
        assert!(estimate_remaining_ms(1000, 1000, std::time::Duration::from_millis(100)).is_none());
    }

    #[test]
    fn sri_prefix_selects_the_algorithm() {
        assert_eq!(
            parse_sri("sha512-abc=").unwrap().algorithm,
            SriAlgorithm::Sha512
        );
        assert_eq!(
            parse_sri("sha256-abc=").unwrap().algorithm,
            SriAlgorithm::Sha256
        );
    }

    #[test]
    fn sri_without_a_prefix_is_rejected() {
        // 无前缀时无从得知算法；宁可拒绝也不能按错的算法算（§3.8 D-2、D-G）
        let e = parse_sri("YWJjZA==").unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
        assert!(
            e.reason.contains("摘要"),
            "原因要说明是摘要格式问题：{}",
            e.reason
        );
    }

    #[test]
    fn sri_with_an_unsupported_algorithm_is_rejected() {
        let e = parse_sri("sha1-abc=").unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
    }

    #[test]
    fn sri_with_invalid_base64_is_rejected() {
        let e = parse_sri("sha512-!!!not-base64!!!").unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
    }

    #[test]
    fn missing_integrity_refuses_the_install_with_its_own_code() {
        // §3.8 D-2：integrity 缺失时**拒绝安装**，不得跳过校验（FR-1.5）
        let e = verify_integrity(None, b"whatever").unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_NO_INTEGRITY);
        assert!(!e.retryable, "缺摘要不是靠重试能解决的");
    }

    /// base64 编码（仅测试用）：仓库不引入 base64 依赖，SRI 解码在 production 内自备。
    fn encode_base64(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(TABLE[((n >> 18) & 63) as usize] as char);
            out.push(TABLE[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    /// 用真实算法算出摘要，拼成 SRI 字符串（不硬编码任何 hex 常量）。
    fn sri_of(bytes: &[u8], algorithm: SriAlgorithm) -> String {
        use sha2::Digest as _;
        let b64 = match algorithm {
            SriAlgorithm::Sha512 => encode_base64(&sha2::Sha512::digest(bytes)),
            SriAlgorithm::Sha256 => encode_base64(&sha2::Sha256::digest(bytes)),
        };
        format!("{}-{b64}", algorithm.as_str())
    }

    #[test]
    fn matching_sha512_integrity_passes() {
        let bytes = b"pretend this is a tarball";
        let sri = sri_of(bytes, SriAlgorithm::Sha512);
        assert!(sri.starts_with("sha512-"));
        verify_integrity(Some(&sri), bytes).expect("摘要一致必须通过");
    }

    #[test]
    fn matching_sha256_integrity_passes() {
        let bytes = b"another tarball";
        let sri = sri_of(bytes, SriAlgorithm::Sha256);
        verify_integrity(Some(&sri), bytes).expect("两种算法都要能用（§3.8 D-2）");
    }

    #[test]
    fn mismatching_integrity_is_rejected_with_reason_and_next_step() {
        let sri = sri_of(b"the real tarball", SriAlgorithm::Sha512);
        let e = verify_integrity(Some(&sri), b"a tampered tarball").unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
        assert!(e.retryable, "摘要不匹配可以重试（§3.2）");
        assert!(!e.next_step.is_empty(), "失败必须给出下一步（NFR-1）");
        assert!(e.reason.contains("sha512"), "原因要写明用的是哪种算法");
    }

    #[test]
    fn comparison_is_on_decoded_bytes_not_on_the_base64_string() {
        // 同一个摘要的等价 base64 写法（补上 / 去掉填充）必须判为一致——
        // 字符串比较会把它误判为不匹配（§3.8 D-2「按字节比较」）。
        let bytes = b"byte-wise comparison matters";
        let padded = sri_of(bytes, SriAlgorithm::Sha512);
        let unpadded = padded.trim_end_matches('=').to_string();
        assert_ne!(padded, unpadded, "两种写法字符串不同");
        verify_integrity(Some(&padded), bytes).unwrap();
        verify_integrity(Some(&unpadded), bytes).expect("补位差异不得导致校验失败");
    }

    use super::fake_registry;

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        // 每个用例一个独立目录：多个用例会经 request_for 并发调用本函数，
        // 固定目录名会在 remove/create 之间互相踩（实测偶发 AlreadyExists）。
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dsh-manager-installer-test-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn known_sri_vectors_decode_independently_of_the_test_encoder() {
        // 已知向量由独立工具（Node crypto）算出，避免「自编自解」的回路掩盖解码器缺陷。
        verify_integrity(
            Some("sha256-uU0nuZNNPgilLlLX2n2r+sSE7+N6U4DukIj3rOLvzek="),
            b"hello world",
        )
        .unwrap();
        verify_integrity(
            Some("sha512-MJ7MSJwS1utMxA9QyQLytNDtd+5RGnx6m808qG1M2G+YndNbxf9JlnDaNCVbRbDP2DDoH2Bdz33FVC6TrpzXbw=="),
            b"hello world",
        )
        .unwrap();
    }

    fn release_for(
        version: &str,
        integrity: Option<String>,
    ) -> crate::ports::registry::PackageRelease {
        crate::ports::registry::PackageRelease {
            version: semver::Version::parse(version).unwrap(),
            published_at_iso: "2026-10-02T08:11:32.114Z".to_string(),
            integrity,
            tarball_url: "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh.tgz".to_string(),
            unpacked_size: Some(91_756),
        }
    }

    fn request_for(version: &str, available: u64) -> InstallRequest {
        InstallRequest {
            channel: "latest".to_string(),
            release: release_for(version, Some("sha512-abc=".to_string())),
            dest: tmp_dir("req-placeholder").join("@deepseek-ai-dsh-0.2.0-rc.2.tgz"),
            available_bytes: available,
            tarball_bytes: Some(20_000),
        }
    }

    #[test]
    fn insufficient_space_is_refused_before_any_download() {
        // FR-1.5：磁盘空间不足须**提前拒绝**并提示；需要 91756 + 20000 = 111756 字节
        let req = request_for("0.2.0-rc.2", 100_000);
        let e = check_space(&req).unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INSUFFICIENT_SPACE);
        assert!(e.retryable, "腾出空间后可以重试");
        assert!(
            e.reason.contains("磁盘"),
            "原因要说清是磁盘空间：{}",
            e.reason
        );
        assert!(!e.next_step.is_empty(), "必须给出下一步（NFR-1）");
    }

    #[test]
    fn enough_space_passes_and_reports_the_requirement() {
        let req = request_for("0.2.0-rc.2", 111_756);
        assert_eq!(req.space_required(), 111_756, "边界值必须恰好通过");
        check_space(&req).expect("空间恰好够时必须通过");
        let req2 = request_for("0.2.0-rc.2", 111_755);
        assert!(check_space(&req2).is_err(), "少一个字节必须拒绝");
    }

    #[test]
    fn unknown_tarball_size_still_checks_the_unpacked_size() {
        let mut req = request_for("0.2.0-rc.2", 91_756);
        req.tarball_bytes = None;
        assert_eq!(req.space_required(), 91_756);
        check_space(&req).unwrap();
        req.available_bytes = 91_755;
        assert!(check_space(&req).is_err());
    }

    #[test]
    fn non_https_tarball_is_refused() {
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        req.release.tarball_url = "http://registry.npmjs.org/x.tgz".to_string();
        let e = check_space(&req).unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
        assert!(
            e.reason.contains("HTTPS"),
            "原因要指明传输必须加密：{}",
            e.reason
        );
    }

    #[test]
    fn available_space_helper_reads_a_real_volume() {
        // 只断言「能读出非零值」：具体容量依机器而定，不做脆弱断言（审计重要-13 的同精神）
        let here = std::env::temp_dir();
        assert!(
            available_space_for(&here) > 0,
            "临时目录所在卷必须能读出可用空间"
        );
    }

    /// 假注册表 + 一次安装请求。dest 由调用方给（§3.6：路径由 DataPaths::tarball 产出）。
    fn fake_with(
        req: &mut InstallRequest,
        download: fake_registry::FakeDownloadResult,
        name: &str,
    ) -> fake_registry::FakeRegistry {
        let dir = tmp_dir(name);
        req.dest = dir.join("@deepseek-ai-dsh-0.2.0-rc.2.tgz");
        let url = req.release.tarball_url.clone();
        fake_registry::FakeRegistry::new().with_download(&url, download)
    }

    #[tokio::test]
    async fn download_goes_through_the_port_and_reports_percent_ending_at_100() {
        // 审计阻断-11：必须经 RegistrySource::download；假实现记录的调用次数即证据
        let payload = b"0123456789".repeat(100); // 1000 字节
        let sri = sri_of(&payload, SriAlgorithm::Sha512);
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        req.release.integrity = Some(sri);
        let registry = fake_with(
            &mut req,
            Ok(fake_registry::FakeDownload::Loop {
                size: 1000,
                chunk: 100,
            }),
            "port-download",
        );

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
        let collecting = {
            let seen = seen.clone();
            move |p: TaskProgress| seen.lock().unwrap().push(p.percent)
        };
        let outcome = download_and_verify(
            &registry,
            &req,
            crate::m0::cancel::CancelToken::new_for_test(),
            collecting,
        )
        .await
        .unwrap();

        assert_eq!(outcome.bytes, 1000);
        assert!(outcome.path.exists(), "成功后安装包必须落盘");
        assert_eq!(registry.download_calls(), 1, "下载必须发生在端口上（§3.3）");
        let percents = seen.lock().unwrap().clone();
        assert!(percents.len() >= 2, "必须周期性推送进度：{percents:?}");
        assert!(
            percents.windows(2).all(|w| w[1] >= w[0]),
            "百分比单调不减：{percents:?}"
        );
        assert_eq!(percents.first(), Some(&0));
        assert_eq!(percents.last(), Some(&100), "必须终止于 100：{percents:?}");
    }

    #[tokio::test]
    async fn convert_progress_is_monotonic_and_estimates_remaining_time() {
        // 「字节数 → 百分比 + 剩余时间」的换算在 M1（§3.3.1 分层约束），这条用例是它的家
        let mut last = 0u32;
        let mut seen = Vec::new();
        for (done, ms) in [
            (0u64, 0u64),
            (100, 10),
            (250, 25),
            (250, 30),
            (999, 90),
            (1000, 100),
        ] {
            let p = convert_progress(done, Some(1000), std::time::Duration::from_millis(ms), last);
            assert!(p.percent >= last, "百分比不得回退：{} < {last}", p.percent);
            last = p.percent;
            seen.push(p.percent);
        }
        assert_eq!(seen.last(), Some(&100), "必须终止于 100");
        let half = convert_progress(500, Some(1000), std::time::Duration::from_millis(500), 50);
        let remaining = half.remaining_ms.expect("速率已知时必须给出剩余时间");
        assert!(
            (400..=600).contains(&remaining),
            "剩余时间估算不合理：{remaining}"
        );
    }

    #[tokio::test]
    async fn integrity_mismatch_marks_unusable_and_deletes_the_file() {
        let payload = b"real tarball bytes".to_vec();
        let sri = sri_of(b"different bytes", SriAlgorithm::Sha512);
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        req.release.integrity = Some(sri);
        let registry = fake_with(
            &mut req,
            Ok(fake_registry::FakeDownload::Bytes(payload)),
            "mismatch",
        );
        let dest = req.dest.clone();
        let e = download_and_verify(
            &registry,
            &req,
            crate::m0::cancel::CancelToken::new_for_test(),
            |_p: TaskProgress| {},
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_INTEGRITY_MISMATCH);
        // §3.6 消歧：校验不通过的文件已证明不可用，必须删除
        assert!(!dest.exists(), "校验不过的文件不得留在临时目录供安装");
    }

    #[tokio::test]
    async fn missing_integrity_refuses_before_any_download() {
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        req.release.integrity = None;
        let registry = fake_with(
            &mut req,
            Ok(fake_registry::FakeDownload::Bytes(b"x".to_vec())),
            "no-integrity",
        );
        let e = download_and_verify(
            &registry,
            &req,
            crate::m0::cancel::CancelToken::new_for_test(),
            |_p: TaskProgress| {},
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_NO_INTEGRITY);
        assert_eq!(registry.download_calls(), 0, "拒绝时必须还没发起下载");
        assert!(!req.dest.exists(), "拒绝时不得留下任何文件");
    }

    #[tokio::test]
    async fn an_already_cancelled_token_stops_before_downloading() {
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        let registry = fake_with(
            &mut req,
            Ok(fake_registry::FakeDownload::Bytes(b"x".to_vec())),
            "cancelled",
        );
        let token = crate::m0::cancel::CancelToken::new_for_test();
        token.cancel();
        let e = download_and_verify(&registry, &req, token, |_p: TaskProgress| {})
            .await
            .unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_CANCELLED);
        assert_eq!(registry.download_calls(), 0, "已取消时不得发起下载");
        assert!(!req.dest.exists(), "取消后不得残留临时文件（NFR-2）");
    }

    #[tokio::test]
    async fn cancelling_mid_download_deletes_the_partial_file() {
        let mut req = request_for("0.2.0-rc.2", u64::MAX);
        req.tarball_bytes = Some(2_000_000);
        let registry = fake_with(
            &mut req,
            Ok(fake_registry::FakeDownload::Loop {
                size: 2_000_000,
                chunk: 100_000,
            }),
            "cancel-mid",
        );
        let dest = req.dest.clone();
        let token = crate::m0::cancel::CancelToken::new_for_test();
        // 第一个「非零进度」事件到达时取消：等价于用户在下载中途点了取消
        let cancel_after_first_chunk = {
            let token = token.clone();
            let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            move |p: TaskProgress| {
                if p.percent > 0 && !seen.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    token.cancel();
                }
            }
        };
        let e = download_and_verify(&registry, &req, token, cancel_after_first_chunk)
            .await
            .unwrap_err();
        assert_eq!(e.code, crate::error::DOWNLOAD_CANCELLED);
        // 验收要求「原因 + 下一步」，且取消后允许重新安装（§3.2：download.cancelled 可重试）
        assert!(!e.reason.is_empty(), "取消必须说明原因：{}", e.reason);
        assert!(!e.next_step.is_empty(), "取消必须给出下一步（NFR-1）");
        assert!(e.retryable, "取消后允许重新点击安装（§3.2）");
        assert!(!dest.exists(), "取消后临时文件必须被删除（FR-1.5、NFR-2）");
    }
}

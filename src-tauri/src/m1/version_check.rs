//! 版本检查（主计划 §3.4 命令 1、§3.8 D-1）。
//!
//! 本模块只消费端口与纯数据：不自己探测包管理器、不探测来源（§2.3 依赖方向），
//! 因此来源结论以 `OriginFacts` 注入。

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::error::AppError;
use crate::error::CmdResult;
use crate::m1::channels::{ChannelCacheEntry, ChannelDef, ChannelsCache, by_id};
use crate::m1::compare::{Verdict, verdict};
use crate::ports::package_manager::PmKind;
use crate::ports::registry::{ChannelProbe, PackageRelease, RegistrySource};

/// 来源识别的四值结论。**§3.3.1 冻结**：定义在 M1，形状与取值一字不改——
/// M1 不得 use M2 的任何实现（§2.3 依赖方向），T-O2 只负责把结论映射成这个形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OriginVerdict {
    PackageManager(PmKind),
    OfficialInstaller,
    Portable,
    Unidentified,
}

/// M1 需要的来源事实。**§3.3.1（§7.4 #28 修订）冻结**：
/// local_version 与 missing 由 T-O2 填充，M1 只消费、不探测。
#[derive(Debug, Clone)]
pub struct OriginFacts {
    pub verdict: OriginVerdict,
    /// 来源识别缺失的依据（M2 的 OriginReport.missing 映射而来），M1 仅透传
    pub missing: Vec<String>,
    /// 本机已安装的 `@deepseek-ai/dsh` 版本；来源识别写入，M1 只消费
    pub local_version: Option<semver::Version>,
}

impl OriginFacts {
    /// 已确认由某个包管理器安装。`local_version` 为该包管理器报告的已装版本。
    pub fn package_manager(kind: PmKind, local_version: Option<&semver::Version>) -> Self {
        Self {
            verdict: OriginVerdict::PackageManager(kind),
            missing: Vec::new(),
            local_version: local_version.cloned(),
        }
    }

    /// 无法确认来源。`missing` 说明还缺什么依据（FR-2.3）。
    pub fn unidentified(missing: &str, local_version: Option<&semver::Version>) -> Self {
        Self {
            verdict: OriginVerdict::Unidentified,
            missing: vec![missing.to_string()],
            local_version: local_version.cloned(),
        }
    }

    /// 版本对比是否适用：只有「包管理器安装」才适用（FR-1.9、设计文档 §4.6、§8.2）。
    pub fn version_applicable(&self) -> bool {
        matches!(self.verdict, OriginVerdict::PackageManager(_))
    }

    /// 不适用的原因，必须区分「确定不属于」与「无法确认」两种情形（设计文档 §4.6）。
    pub fn not_applicable_reason(&self) -> Option<String> {
        match self.verdict {
            OriginVerdict::PackageManager(_) => None,
            OriginVerdict::OfficialInstaller => Some(
                "本机 DSH 由官方安装器安装，不属于 NPM 包 @deepseek-ai/dsh，\
                 本工具的版本管理对该安装方式不适用。请到「安装来源」页查看判定依据。"
                    .to_string(),
            ),
            OriginVerdict::Portable => Some(
                "本机 DSH 是便携版，不属于 NPM 包 @deepseek-ai/dsh，\
                 本工具的版本管理对该安装方式不适用。请到「安装来源」页查看判定依据。"
                    .to_string(),
            ),
            OriginVerdict::Unidentified => Some(
                "无法确认本机 DSH 是否属于 NPM 包 @deepseek-ai/dsh（来源识别尚未给出结论），\
                 因此不给出可能错误的版本对比。请到「安装来源」页查看还缺什么。"
                    .to_string(),
            ),
        }
    }
}

/// 来源事实的共享托管状态：T-O2 写入、check_updates 读取。
///
/// 不放进 AppState：§3.3.2 的字段表是冻结的，来源事实是 T-O2 的私有装配状态。
pub type SharedOriginFacts = std::sync::Arc<std::sync::RwLock<OriginFacts>>;

/// 通道三态。与 §3.6 的 ChannelStatus.state 取值一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelState {
    Published,
    Unpublished,
    Failed,
}

impl ChannelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelState::Published => "published",
            ChannelState::Unpublished => "unpublished",
            ChannelState::Failed => "failed",
        }
    }

    /// 从缓存里的字符串还原。**不叫 `from_str`**：那会触发 `clippy::should_implement_trait`，
    /// 而 §1.4 要求 `cargo clippy -- -D warnings` 必须干净（审计重要-13）。
    pub fn from_cache_str(s: &str) -> Self {
        match s {
            "published" => ChannelState::Published,
            "unpublished" => ChannelState::Unpublished,
            _ => ChannelState::Failed,
        }
    }
}

impl serde::Serialize for ChannelState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// 一条通道对外的状态（§3.6）。字段名 camelCase 由 §3.4 的结构体约定保证。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStatus {
    pub channel: String,
    pub state: ChannelState,
    pub version: Option<String>,
    pub published_at_iso: Option<String>,
    pub tarball_url: Option<String>,
    pub last_success_at_ms: Option<u64>,
    pub last_error: Option<AppError>,
}

/// check_updates 的返回（§3.4 命令 1）。state 以小写字符串序列化。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckUpdatesReport {
    pub channels: Vec<ChannelStatus>,
    pub local_version: Option<String>,
    pub version_applicable: bool,
    pub not_applicable_reason: Option<String>,
    pub last_success_at_ms: Option<u64>,
    /// 每条通道的结论，**由后端算好**（`newer` / `up_to_date` / `not_applicable`）。
    ///
    /// 前端只做文案映射，**不得自己写一份 semver 比较**：FR-1.3 要求按语义化版本规范判断，
    /// 前端重写一份就是第二个真源，且预发布段容易退化成字符串比较（审计重要-8）。
    pub verdicts: BTreeMap<String, String>,
}

/// 自动安装提示（§3.5 事件 update:available 的载荷来源，判定在 T-V4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotice {
    pub channel: String,
    pub version: String,
    pub needs_confirm: bool,
}

/// 当前墙上时钟的纪元毫秒（§3.5 时间表示）。复用 M0 的时钟，不引入时间库。
pub fn now_epoch_ms() -> u64 {
    crate::m0::now_ms()
}

/// 并发探测所有通道，整体受 `overall` 约束。
///
/// 用 `biased;` + 把 `sleep` 放在第一个分支：保证每个探测 future 至少被轮询一次。
/// 已完成的结果逐条收集，超时后返回**已完成的部分结果**，未完成的通道按超时失败处理（NFR-4）。
async fn probe_all(
    registry: &dyn RegistrySource,
    defs: &[&'static ChannelDef],
    overall: Duration,
) -> Vec<(&'static str, ChannelProbe)> {
    let mut pending = FuturesUnordered::new();
    for (index, def) in defs.iter().enumerate() {
        pending.push(async move { (index, registry.probe(def.id).await) });
    }
    let mut results: Vec<Option<ChannelProbe>> = defs.iter().map(|_| None).collect();
    let deadline = tokio::time::sleep(overall);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            _ = &mut deadline => break,
            next = pending.next() => match next {
                Some((index, probe)) => results[index] = Some(probe),
                None => break,
            },
        }
    }
    defs.iter()
        .zip(results)
        .map(|(def, slot)| {
            let probe = slot.unwrap_or_else(|| {
                ChannelProbe::Failed(
                    AppError::new(
                        crate::error::NETWORK_OFFLINE,
                        format!(
                            "检查通道「{}」超时（整体超过 {} 秒）。",
                            def.label_zh,
                            overall.as_secs()
                        ),
                        "检查网络连接或系统代理设置后重试。",
                    )
                    .retryable(true),
                )
            });
            (def.id, probe)
        })
        .collect()
}

/// 把一次探测结果与上次缓存合并为 ChannelStatus。
///
/// **失败时保留上次成功的 version / publishedAtIso / lastSuccessAtMs，不覆盖**（E2E-2）。
fn merge(
    channel: &str,
    probe: ChannelProbe,
    previous: Option<&ChannelCacheEntry>,
    now_ms: u64,
) -> (ChannelStatus, ChannelCacheEntry) {
    let keep = |prev: Option<&ChannelCacheEntry>| {
        let p = prev?;
        Some((
            p.version.clone(),
            p.published_at_iso.clone(),
            p.tarball_url.clone(),
            p.last_success_at_ms,
        ))
    };
    let (state, version, iso, url, at, err) = match probe {
        ChannelProbe::Published(r) => {
            let PackageRelease {
                version,
                published_at_iso,
                tarball_url,
                ..
            } = r;
            (
                ChannelState::Published,
                Some(version.to_string()),
                Some(published_at_iso),
                Some(tarball_url),
                Some(now_ms),
                None,
            )
        }
        ChannelProbe::Unpublished => (ChannelState::Unpublished, None, None, None, None, None),
        ChannelProbe::Failed(e) => {
            let (v, i, u, a) = keep(previous).unwrap_or((None, None, None, None));
            (ChannelState::Failed, v, i, u, a, Some(e))
        }
    };
    let entry = ChannelCacheEntry {
        channel: channel.to_string(),
        state: state.as_str().to_string(),
        version: version.clone(),
        published_at_iso: iso.clone(),
        tarball_url: url.clone(),
        last_success_at_ms: at,
        last_error: err.clone(),
    };
    let status = ChannelStatus {
        channel: channel.to_string(),
        state,
        version,
        published_at_iso: iso,
        tarball_url: url,
        last_success_at_ms: at,
        last_error: err,
    };
    (status, entry)
}

/// 检查更新：并发探测、三态合并、失败保留、缓存落盘、结论判定。
///
/// 本机版本与来源结论都来自 `facts`：M1 只消费，不探测。
pub async fn check_updates(
    registry: &dyn RegistrySource,
    facts: &OriginFacts,
    channel_ids: &[String],
    previous: &ChannelsCache,
    cache_file: &std::path::Path,
    overall: Duration,
) -> CmdResult<CheckUpdatesReport> {
    let mut defs = Vec::with_capacity(channel_ids.len());
    for id in channel_ids {
        match by_id(id) {
            Some(def) => defs.push(def),
            None => {
                return Err(AppError::new(
                    crate::error::REGISTRY_PARSE,
                    format!("请求了未定义的通道：{id}。"),
                    "请从界面提供的通道列表中选择。",
                )
                .retryable(false));
            }
        }
    }
    let now_ms = now_epoch_ms();
    let probed = probe_all(registry, &defs, overall).await;
    let mut statuses = Vec::with_capacity(probed.len());
    let mut cache = ChannelsCache {
        entries: Vec::new(),
    };
    for (channel, probe) in probed {
        let (status, entry) = merge(channel, probe, previous.get(channel), now_ms);
        statuses.push(status);
        cache.upsert(entry);
    }
    // 缓存只是加速；写不进去不能影响检查结果（NFR-7）
    if let Err(e) = cache.save(cache_file) {
        log::warn!("通道缓存写入失败（本次结果仍有效）：{}", e.reason);
    }
    let applicable = facts.version_applicable();
    let local_version = if applicable {
        facts.local_version.as_ref().map(|v| v.to_string())
    } else {
        None
    };
    let last_success_at_ms = statuses.iter().filter_map(|s| s.last_success_at_ms).max();
    // 结论在这里一次算清，界面只负责显示（FR-1.2/FR-1.3 的唯一真源）
    let mut verdicts = BTreeMap::new();
    for status in &statuses {
        let v = channel_verdict(
            facts.local_version.as_ref(),
            status.version.as_deref(),
            applicable,
        );
        verdicts.insert(status.channel.clone(), v.as_str().to_string());
    }
    Ok(CheckUpdatesReport {
        channels: statuses,
        local_version,
        version_applicable: applicable,
        not_applicable_reason: facts.not_applicable_reason(),
        last_success_at_ms,
        verdicts,
    })
}

/// 结论：本机版本与某条通道最新版本的对比（T-V2 的 verdict 薄封装）。
pub fn channel_verdict(
    local: Option<&semver::Version>,
    remote: Option<&str>,
    applicable: bool,
) -> Verdict {
    let Some(remote_str) = remote else {
        return Verdict::NotApplicable;
    };
    let Ok(remote) = semver::Version::parse(remote_str) else {
        return Verdict::NotApplicable;
    };
    verdict(local, &remote, applicable)
}

/// 自动安装提示：**只有该通道最新版本高于本机版本时才产出**（FR-1.4，T-V4 判定）。
pub fn update_notice(
    channel: &str,
    remote: Option<&str>,
    local: Option<&semver::Version>,
    applicable: bool,
    confirm_before_install: bool,
) -> Option<UpdateNotice> {
    if channel_verdict(local, remote, applicable) != Verdict::Newer {
        return None;
    }
    Some(UpdateNotice {
        channel: channel.to_string(),
        version: remote?.to_string(),
        needs_confirm: confirm_before_install,
    })
}

#[cfg(test)]
#[path = "../adapters/fake/fake_registry.rs"]
mod fake_registry;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NETWORK_OFFLINE;
    use crate::m1::channels::{CHANNELS, ChannelCacheEntry, ChannelsCache};
    use crate::ports::package_manager::PmKind;
    use crate::ports::registry::{ChannelProbe, PackageRelease};
    use fake_registry::{FakeProbe, FakeRegistry};
    use std::path::PathBuf;
    use std::time::Duration;

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap_or_else(|e| panic!("测试夹具版本串 {s} 非法：{e}"))
    }

    fn rel(version: &str, iso: &str) -> PackageRelease {
        PackageRelease {
            version: v(version),
            published_at_iso: iso.to_string(),
            integrity: Some("sha512-abc".to_string()),
            tarball_url: format!("https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-{version}.tgz"),
            unpacked_size: Some(91756),
        }
    }

    fn cache_entry(channel: &str, version: &str, iso: &str, at_ms: u64) -> ChannelCacheEntry {
        ChannelCacheEntry {
            channel: channel.to_string(),
            state: "published".to_string(),
            version: Some(version.to_string()),
            published_at_iso: Some(iso.to_string()),
            tarball_url: Some(format!(
                "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-{version}.tgz"
            )),
            last_success_at_ms: Some(at_ms),
            last_error: None,
        }
    }

    fn cache_file(tag: &str) -> PathBuf {
        crate::m0::paths::temp_root(tag).join("channels.json")
    }

    fn all_channels() -> Vec<String> {
        CHANNELS.iter().map(|c| c.id.to_string()).collect()
    }

    fn empty() -> ChannelsCache {
        ChannelsCache {
            entries: Vec::new(),
        }
    }

    fn npm_facts(local: Option<&semver::Version>) -> OriginFacts {
        OriginFacts::package_manager(PmKind::Npm, local)
    }

    // ---- 三态组合 ----

    #[tokio::test]
    async fn all_published_yields_three_published_states() {
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe(
                "next",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe(
                "alpha",
                vec![FakeProbe::Published(rel(
                    "0.2.1-alpha.4",
                    "2026-10-14T09:00:00.000Z",
                ))],
            );
        let local = v("0.1.0-rc.8");
        let out = check_updates(
            &fake,
            &npm_facts(Some(&local)),
            &all_channels(),
            &empty(),
            &cache_file("all-published"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(out.channels.len(), 3);
        assert!(
            out.channels
                .iter()
                .all(|c| c.state == ChannelState::Published)
        );
        assert_eq!(out.channels[2].version.as_deref(), Some("0.2.1-alpha.4"));
        assert!(out.channels.iter().all(|c| c.last_error.is_none()));
        assert!(out.channels.iter().all(|c| c.last_success_at_ms.is_some()));
        assert!(out.version_applicable);
        assert!(out.not_applicable_reason.is_none());
        assert_eq!(out.local_version.as_deref(), Some("0.1.0-rc.8"));
        assert_eq!(
            out.verdicts.get("latest").map(String::as_str),
            Some("newer")
        );
        assert_eq!(out.verdicts.get("alpha").map(String::as_str), Some("newer"));
    }

    #[tokio::test]
    async fn unpublished_channel_is_not_an_error() {
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe(
                "alpha",
                vec![FakeProbe::Published(rel(
                    "0.2.1-alpha.4",
                    "2026-10-14T09:00:00.000Z",
                ))],
            );
        let local = v("0.2.1-alpha.4");
        let out = check_updates(
            &fake,
            &npm_facts(Some(&local)),
            &all_channels(),
            &empty(),
            &cache_file("unpublished"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let next = out.channels.iter().find(|c| c.channel == "next").unwrap();
        assert_eq!(next.state, ChannelState::Unpublished);
        assert!(next.version.is_none());
        assert!(
            next.last_error.is_none(),
            "未发布不算错误（FR-1.1、SRS R2）"
        );
        assert_eq!(
            out.verdicts.get("next").map(String::as_str),
            Some("not_applicable"),
            "没有版本事实就不产出对比结论"
        );
    }

    #[tokio::test]
    async fn one_failed_channel_keeps_the_other_two_results() {
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe(
                "alpha",
                vec![FakeProbe::Failed(
                    NETWORK_OFFLINE.to_string(),
                    "无法连接到包注册表：连接被拒绝。".to_string(),
                )],
            );
        let local = v("0.2.1-alpha.4");
        let out = check_updates(
            &fake,
            &npm_facts(Some(&local)),
            &all_channels(),
            &empty(),
            &cache_file("one-failed"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let alpha = out.channels.iter().find(|c| c.channel == "alpha").unwrap();
        assert_eq!(alpha.state, ChannelState::Failed);
        assert_eq!(alpha.last_error.as_ref().unwrap().code, NETWORK_OFFLINE);
        assert!(
            out.channels
                .iter()
                .any(|c| c.state == ChannelState::Published)
        );
        assert!(
            out.channels
                .iter()
                .any(|c| c.state == ChannelState::Unpublished)
        );
    }

    // ---- E2E-2：失败保留上次成功，不覆盖 ----

    #[tokio::test]
    async fn failure_keeps_previous_success_and_does_not_advance_its_time() {
        let prev = ChannelsCache {
            entries: vec![cache_entry(
                "latest",
                "0.2.0-rc.2",
                "2026-10-02T08:11:32.114Z",
                1_760_000_000_000,
            )],
        };
        let cache = cache_file("keep-previous");
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Failed(
                    NETWORK_OFFLINE.to_string(),
                    "无法连接到包注册表：连接被拒绝。".to_string(),
                )],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe("alpha", vec![FakeProbe::Unpublished]);
        let local = v("0.2.1-alpha.4");
        let out = check_updates(
            &fake,
            &npm_facts(Some(&local)),
            &all_channels(),
            &prev,
            &cache,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let latest = out.channels.iter().find(|c| c.channel == "latest").unwrap();
        assert_eq!(latest.state, ChannelState::Failed);
        assert_eq!(
            latest.version.as_deref(),
            Some("0.2.0-rc.2"),
            "失败不得清掉上次成功的版本"
        );
        assert_eq!(
            latest.published_at_iso.as_deref(),
            Some("2026-10-02T08:11:32.114Z"),
            "失败不得清掉上次成功的发布日期"
        );
        assert_eq!(
            latest.last_success_at_ms,
            Some(1_760_000_000_000),
            "失败不得把上次成功时间刷新成当前时间"
        );
        assert_eq!(out.last_success_at_ms, Some(1_760_000_000_000));
        assert!(latest.last_error.is_some());

        // 冷启动也要能看到「失败 + 上次成功」：缓存里必须留下状态与错误
        let (reloaded, err) = ChannelsCache::load(&cache);
        assert!(err.is_none());
        let entry = reloaded.get("latest").unwrap();
        assert_eq!(entry.state, "failed");
        assert_eq!(entry.version.as_deref(), Some("0.2.0-rc.2"));
        assert_eq!(entry.last_error.as_ref().unwrap().code, NETWORK_OFFLINE);
    }

    #[tokio::test]
    async fn success_advances_last_success_time_and_writes_the_cache() {
        let prev = ChannelsCache {
            entries: vec![cache_entry(
                "latest",
                "0.1.0-rc.8",
                "2026-09-24T10:00:00.000Z",
                1_700_000_000_000,
            )],
        };
        let cache = cache_file("advance");
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe("alpha", vec![FakeProbe::Unpublished]);
        let local = v("0.2.1-alpha.4");
        let out = check_updates(
            &fake,
            &npm_facts(Some(&local)),
            &all_channels(),
            &prev,
            &cache,
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        let latest = out.channels.iter().find(|c| c.channel == "latest").unwrap();
        assert_eq!(latest.version.as_deref(), Some("0.2.0-rc.2"));
        assert!(
            latest.last_success_at_ms.unwrap() > 1_700_000_000_000,
            "成功必须刷新上次成功时间"
        );
        let (reloaded, err) = ChannelsCache::load(&cache);
        assert!(err.is_none());
        let entry = reloaded.get("latest").unwrap();
        assert_eq!(
            entry.version.as_deref(),
            Some("0.2.0-rc.2"),
            "每次检查都要落盘（§3.6：缓存可删、重建即可）"
        );
        // 缓存里的 state 字符串必须能还原成三态，否则它就是死代码
        assert_eq!(
            ChannelState::from_cache_str(&entry.state),
            ChannelState::Published
        );
    }

    #[tokio::test]
    async fn cache_write_failure_does_not_fail_the_check() {
        let fake = FakeRegistry::new()
            .with_probe("latest", vec![FakeProbe::Unpublished])
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe("alpha", vec![FakeProbe::Unpublished]);
        let bad = cache_file("cache-fail");
        std::fs::create_dir_all(&bad).unwrap(); // 用目录占住文件路径，写入必定失败
        let out = check_updates(
            &fake,
            &npm_facts(None),
            &all_channels(),
            &empty(),
            &bad,
            Duration::from_secs(10),
        )
        .await;
        assert!(out.is_ok(), "缓存写不进去不能让检查整体失败（NFR-7）");
    }

    // ---- versionApplicable = false：不产出结论 ----

    #[tokio::test]
    async fn not_applicable_keeps_facts_but_produces_no_conclusion() {
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe(
                "alpha",
                vec![FakeProbe::Published(rel(
                    "0.2.1-alpha.4",
                    "2026-10-14T09:00:00.000Z",
                ))],
            );
        let local = v("0.2.1-alpha.4");
        let facts = OriginFacts::unidentified("来源识别尚未落地", Some(&local));
        let out = check_updates(
            &fake,
            &facts,
            &all_channels(),
            &empty(),
            &cache_file("not-applicable"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert!(!out.version_applicable);
        assert!(
            out.not_applicable_reason
                .as_deref()
                .unwrap()
                .contains("无法确认"),
            "未识别要如实说「无法确认」，不得猜测（FR-2.3）"
        );
        assert!(out.local_version.is_none(), "不适用时不产出本机版本结论");
        assert_eq!(
            out.channels[0].version.as_deref(),
            Some("0.2.0-rc.2"),
            "版本事实仍如实展示（FR-1.1/FR-1.2）——只是不做对比"
        );
        assert!(
            out.verdicts.values().all(|v| v == "not_applicable"),
            "不适用时不得产出任何对比结论，实际 {:?}",
            out.verdicts
        );
    }

    // ---- 来源事实：适用性判定 ----

    #[test]
    fn origin_facts_say_which_verdicts_make_the_comparison_applicable() {
        let local = v("0.2.1-alpha.4");
        let pm = npm_facts(Some(&local));
        assert!(pm.version_applicable());
        assert!(pm.not_applicable_reason().is_none());
        assert!(pm.missing.is_empty());
        assert_eq!(pm.local_version.as_ref(), Some(&local));

        let installer = OriginFacts {
            verdict: OriginVerdict::OfficialInstaller,
            missing: Vec::new(),
            local_version: Some(local.clone()),
        };
        assert!(!installer.version_applicable());
        assert!(
            installer
                .not_applicable_reason()
                .unwrap()
                .contains("官方安装器")
        );

        let portable = OriginFacts {
            verdict: OriginVerdict::Portable,
            missing: Vec::new(),
            local_version: None,
        };
        assert!(!portable.version_applicable());
        assert!(portable.not_applicable_reason().unwrap().contains("便携版"));

        let unidentified = OriginFacts::unidentified("来源识别尚未落地", None);
        assert_eq!(unidentified.missing, vec!["来源识别尚未落地".to_string()]);
        assert!(
            unidentified
                .not_applicable_reason()
                .unwrap()
                .contains("无法确认")
        );
    }

    #[test]
    fn channel_verdict_and_update_notice_never_guess() {
        assert_eq!(
            channel_verdict(Some(&v("0.1.0")), Some("not-a-version"), true),
            Verdict::NotApplicable,
            "非法远端版本不得被当成对比结论"
        );
        let local = v("0.2.0-rc.1");
        let notice = update_notice("latest", Some("0.2.0-rc.2"), Some(&local), true, true)
            .expect("远端更高时必须产出自动安装提示");
        assert_eq!(notice.channel, "latest");
        assert_eq!(notice.version, "0.2.0-rc.2");
        assert!(notice.needs_confirm);
        assert!(update_notice("latest", Some("0.2.0-rc.1"), Some(&local), true, true).is_none());
        assert!(update_notice("latest", Some("0.2.0-rc.2"), None, true, true).is_none());
        assert!(update_notice("latest", Some("0.2.0-rc.2"), Some(&local), false, true).is_none());
    }

    // ---- 输入校验 ----

    #[tokio::test]
    async fn unknown_channel_fails_the_whole_check_without_probing() {
        let fake = FakeRegistry::new();
        let err = check_updates(
            &fake,
            &npm_facts(None),
            &["beta".to_string()],
            &empty(),
            &cache_file("unknown"),
            Duration::from_secs(10),
        )
        .await
        .expect_err("未定义的通道必须整体失败");
        assert_eq!(err.code, crate::error::REGISTRY_PARSE);
        assert!(!err.retryable);
        assert_eq!(fake.probe_calls(), 0, "非法通道不得发起任何探测");
    }

    // ---- 并发与整体超时（NFR-4）----

    #[tokio::test]
    async fn overall_timeout_marks_the_slow_channel_failed_and_keeps_the_fast_one() {
        let fake = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe("alpha", vec![FakeProbe::Unpublished]);
        let slow = SlowChannelRegistry {
            inner: fake,
            slow_channel: "alpha",
            delay: Duration::from_secs(5),
        };
        let started = std::time::Instant::now();
        let out = check_updates(
            &slow,
            &npm_facts(None),
            &all_channels(),
            &empty(),
            &cache_file("timeout"),
            Duration::from_millis(120),
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(1000),
            "整体超时必须真的生效，实际耗时 {:?}",
            started.elapsed()
        );
        let latest = out.channels.iter().find(|c| c.channel == "latest").unwrap();
        assert_eq!(
            latest.state,
            ChannelState::Published,
            "快通道的结果必须保留"
        );
        let alpha = out.channels.iter().find(|c| c.channel == "alpha").unwrap();
        assert_eq!(alpha.state, ChannelState::Failed);
        assert_eq!(alpha.last_error.as_ref().unwrap().code, NETWORK_OFFLINE);
        assert!(alpha.last_error.as_ref().unwrap().reason.contains("超时"));
    }

    #[tokio::test]
    async fn channels_are_probed_concurrently_and_none_is_dropped() {
        // 三个通道各自记录「进入」与「离开」；并发时会出现交错的进入序列，
        // 串行时必然是 进-出-进-出-进-出。
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let inner = FakeRegistry::new()
            .with_probe(
                "latest",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe(
                "next",
                vec![FakeProbe::Published(rel(
                    "0.2.0-rc.2",
                    "2026-10-02T08:11:32.114Z",
                ))],
            )
            .with_probe("alpha", vec![FakeProbe::Unpublished]);
        let reg = LoggingRegistry {
            inner,
            log: log.clone(),
        };
        let out = check_updates(
            &reg,
            &npm_facts(None),
            &all_channels(),
            &empty(),
            &cache_file("concurrent"),
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(out.channels.len(), 3, "并发不得丢通道");
        let entries: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("enter:"))
            .cloned()
            .collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            log.lock().unwrap().first().map(String::as_str),
            Some("enter:latest"),
            "第一个进入的通道必须是第一个被轮询的"
        );
        let interleaved = log
            .lock()
            .unwrap()
            .iter()
            .take(4)
            .filter(|s| s.starts_with("enter:"))
            .count()
            >= 2;
        assert!(
            interleaved,
            "三个通道必须并发进入（串行时前四条里只有一个 enter），实际 {log:?}",
            log = log.lock().unwrap()
        );
    }

    /// 慢通道替身：对指定通道先睡够 delay 再返回，用来验证整体超时。
    struct SlowChannelRegistry {
        inner: FakeRegistry,
        slow_channel: &'static str,
        delay: Duration,
    }

    #[async_trait::async_trait]
    impl crate::ports::registry::RegistrySource for SlowChannelRegistry {
        async fn probe(&self, channel: &str) -> ChannelProbe {
            if channel == self.slow_channel {
                tokio::time::sleep(self.delay).await;
            }
            self.inner.probe(channel).await
        }

        async fn download(
            &self,
            release: &PackageRelease,
            dest: &std::path::Path,
            cancel: crate::m0::cancel::CancelToken,
            progress: crate::m0::ProgressSink,
        ) -> crate::error::CmdResult<()> {
            self.inner.download(release, dest, cancel, progress).await
        }
    }

    /// 记录进入/离开顺序的替身，用来证明探测是并发的。
    struct LoggingRegistry {
        inner: FakeRegistry,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl crate::ports::registry::RegistrySource for LoggingRegistry {
        async fn probe(&self, channel: &str) -> ChannelProbe {
            self.log.lock().unwrap().push(format!("enter:{channel}"));
            let out = self.inner.probe(channel).await;
            self.log.lock().unwrap().push(format!("leave:{channel}"));
            out
        }

        async fn download(
            &self,
            release: &PackageRelease,
            dest: &std::path::Path,
            cancel: crate::m0::cancel::CancelToken,
            progress: crate::m0::ProgressSink,
        ) -> crate::error::CmdResult<()> {
            self.inner.download(release, dest, cancel, progress).await
        }
    }
}

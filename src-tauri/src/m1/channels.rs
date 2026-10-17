//! 通道清单与发布标签映射（主计划 §3.8 D-1）。
//!
//! 新增一条通道只需在 CHANNELS 里加一行，判定逻辑不改。

/// 一个更新通道。id 是对外的稳定标识，dist_tag 是 npm 的发布标签。
pub struct ChannelDef {
    pub id: &'static str,
    pub label_zh: &'static str,
    pub dist_tag: &'static str,
}

/// 已实测 @deepseek-ai/dsh 的发布标签为 latest / next / alpha 三条独立流。
pub const CHANNELS: &[ChannelDef] = &[
    ChannelDef {
        id: "latest",
        label_zh: "稳定通道",
        dist_tag: "latest",
    },
    ChannelDef {
        id: "next",
        label_zh: "预览通道",
        dist_tag: "next",
    },
    ChannelDef {
        id: "alpha",
        label_zh: "先行通道",
        dist_tag: "alpha",
    },
];

/// 按通道标识查定义；未知标识返回 None，调用方负责报错，不静默回落到某条通道。
pub fn by_id(id: &str) -> Option<&'static ChannelDef> {
    CHANNELS.iter().find(|c| c.id == id)
}

/// 版本源固定的包名（FR-1.9）。
pub const PACKAGE_NAME: &str = "@deepseek-ai/dsh";

/// 一条通道在缓存里的持久化状态。
///
/// 字段与主计划 §3.6 的 ChannelStatus 一一对应；错误与状态这两个「当前值」也落盘，
/// 使界面冷启动时能先显示出上次的结果（E2E-1、E2E-2）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelCacheEntry {
    pub channel: String,
    pub state: String,
    pub version: Option<String>,
    pub published_at_iso: Option<String>,
    pub tarball_url: Option<String>,
    pub last_success_at_ms: Option<u64>,
    pub last_error: Option<crate::error::AppError>,
}

/// 通道状态缓存（§3.6：`%LOCALAPPDATA%\dsh-manager\cache\channels.json`，可删、重建即可）。
pub struct ChannelsCache {
    pub entries: Vec<ChannelCacheEntry>,
}

impl ChannelsCache {
    /// 读缓存。文件不存在或损坏时返回空缓存并给出原因，**不阻断启动**（NFR-7）。
    pub fn load(path: &std::path::Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Err(_) => (
                Self {
                    entries: Vec::new(),
                },
                None,
            ),
            Ok(text) => match serde_json::from_str::<Vec<ChannelCacheEntry>>(&text) {
                Ok(entries) => (Self { entries }, None),
                Err(e) => (
                    Self {
                        entries: Vec::new(),
                    },
                    Some(format!("通道缓存无法解析（{}）：{e}", path.display())),
                ),
            },
        }
    }

    pub fn get(&self, channel: &str) -> Option<&ChannelCacheEntry> {
        self.entries.iter().find(|e| e.channel == channel)
    }

    pub fn upsert(&mut self, entry: ChannelCacheEntry) {
        match self.entries.iter_mut().find(|e| e.channel == entry.channel) {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> crate::error::CmdResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::error::AppError::new(
                    crate::error::PATH_PERMISSION_DENIED,
                    format!("无法创建缓存目录 {}：{e}", parent.display()),
                    "检查该目录的写入权限后重试。",
                )
            })?;
        }
        let text = serde_json::to_string_pretty(&self.entries).map_err(|e| {
            crate::error::AppError::new(
                crate::error::REGISTRY_PARSE,
                format!("通道缓存序列化失败：{e}"),
                "删除缓存文件后重试。",
            )
        })?;
        std::fs::write(path, text).map_err(|e| {
            crate::error::AppError::new(
                crate::error::PATH_PERMISSION_DENIED,
                format!("无法写入缓存 {}：{e}", path.display()),
                "检查磁盘剩余空间与目录权限后重试。",
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_have_three_entries_with_unique_ids() {
        assert_eq!(CHANNELS.len(), 3);
        let ids: Vec<&str> = CHANNELS.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["latest", "next", "alpha"]);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "通道标识必须唯一");
    }

    #[test]
    fn channels_have_chinese_labels_and_point_at_their_own_dist_tag() {
        for c in CHANNELS {
            assert!(!c.label_zh.is_empty(), "通道 {} 缺少中文标签", c.id);
            assert!(
                c.label_zh
                    .chars()
                    .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch)),
                "通道 {} 的标签必须是中文，实际为 {}",
                c.id,
                c.label_zh
            );
            assert_eq!(c.id, c.dist_tag, "本包三条标签与通道标识同名（§3.8 D-1）");
        }
    }

    #[test]
    fn by_id_finds_known_and_rejects_unknown() {
        assert_eq!(by_id("alpha").map(|c| c.label_zh), Some("先行通道"));
        assert!(by_id("beta").is_none());
        assert!(by_id("").is_none());
    }

    #[test]
    fn package_name_matches_fr_1_9() {
        assert_eq!(PACKAGE_NAME, "@deepseek-ai/dsh");
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn cache_file(tag: &str) -> std::path::PathBuf {
        crate::m0::paths::temp_root(tag)
            .join("cache")
            .join("channels.json")
    }

    fn entry(version: &str) -> ChannelCacheEntry {
        ChannelCacheEntry {
            channel: "latest".to_string(),
            state: "published".to_string(),
            version: Some(version.to_string()),
            published_at_iso: Some("2026-10-02T08:11:32.114Z".to_string()),
            tarball_url: Some(
                "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.0-rc.2.tgz".to_string(),
            ),
            last_success_at_ms: Some(1_760_000_000_000),
            last_error: None,
        }
    }

    #[test]
    fn missing_file_loads_empty_without_an_error() {
        let (cache, err) = ChannelsCache::load(&cache_file("cache-missing"));
        assert!(cache.entries.is_empty());
        assert!(err.is_none(), "文件不存在不是错误");
    }

    #[test]
    fn corrupt_file_loads_empty_and_reports_the_reason() {
        let p = cache_file("cache-corrupt");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "{ not json").unwrap();
        let (cache, err) = ChannelsCache::load(&p);
        assert!(cache.entries.is_empty());
        assert!(
            err.unwrap().contains("无法解析"),
            "损坏要给出原因，而不是静默回默认值"
        );
    }

    #[test]
    fn cache_round_trip_keeps_version_time_and_last_success() {
        let p = cache_file("cache-roundtrip");
        let mut cache = ChannelsCache {
            entries: Vec::new(),
        };
        cache.upsert(entry("0.2.0-rc.2"));
        cache.save(&p).unwrap();
        assert!(p.is_file(), "save 必须建出父目录并写入 {}", p.display());

        let (loaded, err) = ChannelsCache::load(&p);
        assert!(err.is_none());
        let e = loaded.get("latest").expect("往返后必须还在");
        assert_eq!(e.version.as_deref(), Some("0.2.0-rc.2"));
        assert_eq!(
            e.published_at_iso.as_deref(),
            Some("2026-10-02T08:11:32.114Z")
        );
        assert_eq!(e.last_success_at_ms, Some(1_760_000_000_000));
    }

    #[test]
    fn upsert_replaces_instead_of_appending() {
        let mut cache = ChannelsCache {
            entries: Vec::new(),
        };
        for v in ["0.1.0-rc.8", "0.2.0-rc.2"] {
            cache.upsert(entry(v));
        }
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(
            cache.get("latest").unwrap().version.as_deref(),
            Some("0.2.0-rc.2")
        );
    }

    #[test]
    fn last_error_round_trips_through_the_cache() {
        let p = cache_file("cache-error");
        let mut cache = ChannelsCache {
            entries: Vec::new(),
        };
        let mut failed = entry("0.2.0-rc.2");
        failed.state = "failed".to_string();
        failed.last_error = Some(
            crate::error::AppError::new(
                crate::error::NETWORK_OFFLINE,
                "无法连接到包注册表：连接被拒绝。",
                "检查网络连接或系统代理设置后重试。",
            )
            .retryable(true),
        );
        cache.upsert(failed);
        cache.save(&p).unwrap();

        let (loaded, err) = ChannelsCache::load(&p);
        assert!(err.is_none());
        let e = loaded.get("latest").unwrap();
        assert_eq!(e.state, "failed");
        assert_eq!(
            e.version.as_deref(),
            Some("0.2.0-rc.2"),
            "失败也要能读回上次成功版本"
        );
        assert_eq!(
            e.last_error.as_ref().unwrap().code,
            crate::error::NETWORK_OFFLINE
        );
    }
}

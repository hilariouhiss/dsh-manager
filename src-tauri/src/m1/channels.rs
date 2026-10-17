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

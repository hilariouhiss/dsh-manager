//! 版本比较与结论（主计划 T-V2）。
//!
//! 本文件是 semver crate 的薄封装，不自己写比较（设计文档 §4.2 YAGNI 清单）。

use std::cmp::Ordering;

use semver::Version;

/// 版本比较。**薄封装 semver crate，不自己写比较逻辑**（设计文档 §4.2）。
pub fn compare(local: &Version, remote: &Version) -> Ordering {
    local.cmp_precedence(remote)
}

/// 一条通道的对比结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 远程版本高于本机版本
    Newer,
    /// 本机版本已不低于远程版本
    UpToDate,
    /// 不产出对比结论：本机未安装该包，或本机安装方式不属于该 NPM 包（FR-1.9）
    NotApplicable,
}

impl Verdict {
    /// 是否为一条真正的版本对比结论。NotApplicable 不是。
    pub fn is_conclusion(&self) -> bool {
        !matches!(self, Verdict::NotApplicable)
    }

    /// 给 IPC 用的稳定标识：前端据它做文案映射，不自己判断版本（审计重要-8）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Newer => "newer",
            Verdict::UpToDate => "up_to_date",
            Verdict::NotApplicable => "not_applicable",
        }
    }

    /// 界面文案（简体中文，NFR-1）。
    pub fn text_zh(&self) -> &'static str {
        match self {
            Verdict::Newer => "有新版本",
            Verdict::UpToDate => "已是最新",
            Verdict::NotApplicable => "本工具的版本管理对该安装方式不适用或无法确认",
        }
    }
}

/// 结论判定。
///
/// `applicable == false` 或本机版本未知时**不产出结论**（FR-1.9、设计文档 §4.6）：
/// 该分支不得猜测，也不得给出可能错误的版本对比。
pub fn verdict(local: Option<&Version>, remote: &Version, applicable: bool) -> Verdict {
    if !applicable {
        return Verdict::NotApplicable;
    }
    let Some(local) = local else {
        return Verdict::NotApplicable;
    };
    if compare(local, remote) == Ordering::Less {
        Verdict::Newer
    } else {
        Verdict::UpToDate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering as O;

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap_or_else(|e| panic!("测试夹具版本串 {s} 非法：{e}"))
    }

    // ---- FR-1.3 原文验收：两条必须逐字落地 ----

    #[test]
    fn fr_1_3_rc2_is_newer_than_rc1() {
        assert_eq!(compare(&v("0.2.0-rc.1"), &v("0.2.0-rc.2")), O::Less);
        assert_eq!(compare(&v("0.2.0-rc.2"), &v("0.2.0-rc.1")), O::Greater);
    }

    #[test]
    fn fr_1_3_release_is_newer_than_its_rc() {
        assert_eq!(compare(&v("0.2.0-rc.2"), &v("0.2.0")), O::Less);
        assert_eq!(compare(&v("0.2.0"), &v("0.2.0-rc.2")), O::Greater);
    }

    #[test]
    fn prerelease_is_never_above_the_same_release_version() {
        for pre in [
            "0.2.0-alpha",
            "0.2.0-alpha.1",
            "0.2.0-beta.11",
            "0.2.0-rc.1",
            "0.2.0-rc.2",
            "0.2.0-0",
        ] {
            assert_eq!(
                compare(&v(pre), &v("0.2.0")),
                O::Less,
                "{pre} 不能高于同版本正式版 0.2.0"
            );
        }
    }

    // ---- 真实注册表版本串（2026-10-16 实测存在）----

    #[test]
    fn real_registry_prerelease_pairs() {
        assert_eq!(compare(&v("0.2.1-alpha.4"), &v("0.2.0-rc.2")), O::Greater);
        assert_eq!(compare(&v("0.1.0-rc.6"), &v("0.1.0-rc.7")), O::Less);
        assert_eq!(compare(&v("0.1.0-rc.7"), &v("0.1.0-rc.8")), O::Less);
        assert_eq!(compare(&v("0.1.0-rc.8"), &v("0.2.0-rc.1")), O::Less);
        assert_eq!(compare(&v("0.1.2-alpha.5"), &v("0.1.2-rc.1")), O::Less);
        assert_eq!(compare(&v("0.1.2-rc.1"), &v("0.1.3-alpha.2")), O::Less);
        assert_eq!(compare(&v("0.2.0-rc.2"), &v("0.2.1-alpha.1")), O::Less);
    }

    #[test]
    fn real_registry_same_version_is_equal() {
        // 本机实测为 0.2.1-alpha.4，稳定通道实测为 0.2.0-rc.2
        assert_eq!(compare(&v("0.2.1-alpha.4"), &v("0.2.1-alpha.4")), O::Equal);
    }

    // ---- 数字段与字母段不混比（FR-1.3「必须正确判断」的边界）----

    #[test]
    fn numeric_and_alphanumeric_identifiers_do_not_panic() {
        // semver 规范：数字标识符总是低于字母标识符
        assert_eq!(compare(&v("1.0.0-1"), &v("1.0.0-alpha")), O::Less);
        assert_eq!(compare(&v("1.0.0-alpha"), &v("1.0.0-1")), O::Greater);
        assert_eq!(
            compare(&v("1.0.0-2"), &v("1.0.0-11")),
            O::Less,
            "数字段按数值比较，不是字典序"
        );
        assert_eq!(
            compare(&v("1.0.0-alpha.1"), &v("1.0.0-alpha.beta")),
            O::Less
        );
        assert_eq!(
            compare(&v("1.0.0-alpha"), &v("1.0.0-alpha.1")),
            O::Less,
            "字段少者更低"
        );
    }

    #[test]
    fn build_metadata_is_ignored() {
        assert_eq!(compare(&v("1.0.0+build.1"), &v("1.0.0+build.2")), O::Equal);
        assert_eq!(
            compare(&v("1.0.0-alpha+001"), &v("1.0.0-alpha+002")),
            O::Equal
        );
    }

    #[test]
    fn major_version_beats_any_patch_level() {
        assert_eq!(compare(&v("1.0.0"), &v("0.999.999")), O::Greater);
    }

    // ---- 非法版本串：返回解析错误，不得回落默认值 ----

    #[test]
    fn invalid_version_strings_are_rejected() {
        for bad in ["", "1.0", "v1.0.0", "1.0.0.0", "01.0.0", "1.0.0-", "abc"] {
            assert!(
                semver::Version::parse(bad).is_err(),
                "{bad:?} 必须解析失败，不得被当作默认值"
            );
        }
    }

    #[test]
    fn verdict_helpers() {
        assert_eq!(
            verdict(Some(&v("0.2.0-rc.1")), &v("0.2.0-rc.2"), true),
            Verdict::Newer
        );
        assert_eq!(
            verdict(Some(&v("0.2.0-rc.2")), &v("0.2.0-rc.2"), true),
            Verdict::UpToDate
        );
        assert_eq!(
            verdict(Some(&v("0.3.0")), &v("0.2.0-rc.2"), true),
            Verdict::UpToDate
        );
        assert_eq!(
            verdict(None, &v("0.2.0-rc.2"), true),
            Verdict::NotApplicable
        );
        assert_eq!(
            verdict(Some(&v("0.1.0")), &v("0.2.0-rc.2"), false),
            Verdict::NotApplicable,
            "applicable=false 时不产出对比结论（FR-1.9、设计文档 §4.6）"
        );
    }

    #[test]
    fn verdict_text_is_chinese_and_says_the_next_step() {
        assert_eq!(Verdict::Newer.text_zh(), "有新版本");
        assert_eq!(Verdict::UpToDate.text_zh(), "已是最新");
        let na = Verdict::NotApplicable.text_zh();
        assert!(
            na.contains("不适用") || na.contains("无法确认"),
            "实际为 {na}"
        );
        assert!(
            !Verdict::NotApplicable.is_conclusion(),
            "不适用不是一条版本对比结论"
        );
        assert!(Verdict::Newer.is_conclusion());
    }
}

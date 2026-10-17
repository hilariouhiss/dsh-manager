//! RegistrySource 端口（主计划 §3.3，冻结）。
//!
//! 本文件只放 trait 与数据类型；应用层只依赖这里的类型，不 use adapters::*。

use std::path::Path;

use async_trait::async_trait;

use crate::error::{AppError, CmdResult};

/// 一次通道查询的结果。三态是返回值，不是异常（§3.2 末尾：这三种状态不得出现在 AppError 中）。
pub enum ChannelProbe {
    Published(PackageRelease),
    Unpublished,
    Failed(AppError),
}

/// 一条通道最新发布的全部信息。
#[derive(Debug, Clone)]
pub struct PackageRelease {
    pub version: semver::Version,
    /// 注册表原始 ISO-8601 字符串，后端不解析（§3.5 时间表示）
    pub published_at_iso: String,
    /// SRI 摘要，形如 "sha512-<base64>"；缺失时为 None（§3.8 D-2）
    pub integrity: Option<String>,
    pub tarball_url: String,
    /// 注册表 dist.unpackedSize，用于安装前的空间检查
    pub unpacked_size: Option<u64>,
}

impl PackageRelease {
    /// tarball_url 必须是 HTTPS（FR-1.5「传输过程须加密」，§3.8 D-2 末条）。
    pub fn is_https(&self) -> bool {
        self.tarball_url.starts_with("https://")
    }

    /// 安装前所需空间的下界：解包体积 + 压缩包本身（§3.3.1：所需空间取自 `dist.unpackedSize`）。
    /// `tarball_bytes` 只在调用方**已经知道**压缩包字节数时传入；端口不回报 Content-Length，
    /// 因此生产路径传 `None`，测试用它构造边界值。
    pub fn space_required(&self, tarball_bytes: Option<u64>) -> u64 {
        self.unpacked_size
            .unwrap_or(0)
            .saturating_add(tarball_bytes.unwrap_or(0))
    }
}

/// 注册表端口。应用层只依赖本 trait（§3.3）。
#[async_trait]
pub trait RegistrySource: Send + Sync {
    /// 通道标识 → 该通道最新发布。逐通道返回三态，不因单通道失败而整体失败。
    async fn probe(&self, channel: &str) -> ChannelProbe;

    /// 下载 tarball 到 dest，周期性回调进度，支持取消。
    ///
    /// `cancel` 是 §3.3.1 冻结的 `m0::cancel::CancelToken`（不经注册表的测试令牌用
    /// `CancelToken::new_for_test()`）；`progress` 是 §3.3.1 冻结的 `m0::ProgressSink`，
    /// **只承载原始字节数**——「字节数 → 百分比 + 剩余时间 + 阶段」的换算在 M1（T-V5）。
    ///
    /// **空间检查发生在调用本方法之前**（T-V5 的 `check_space`），因此本方法不再查磁盘。
    /// 本方法是 installer 的**唯一**下载路径：**不得绕过端口直接使用 `m0::net::client()` 下载**。
    async fn download(
        &self,
        release: &PackageRelease,
        dest: &Path,
        cancel: crate::m0::cancel::CancelToken,
        progress: crate::m0::ProgressSink,
    ) -> CmdResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> PackageRelease {
        PackageRelease {
            version: semver::Version::parse("0.2.0-rc.2").unwrap(),
            published_at_iso: "2026-10-02T08:11:32.114Z".to_string(),
            integrity: Some("sha512-abc".to_string()),
            tarball_url: "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.0-rc.2.tgz"
                .to_string(),
            unpacked_size: Some(91756),
        }
    }

    #[test]
    fn channel_probe_is_a_return_value_not_an_exception() {
        let published = ChannelProbe::Published(release());
        let unpublished = ChannelProbe::Unpublished;
        let failed = ChannelProbe::Failed(crate::error::AppError::new(
            crate::error::NETWORK_OFFLINE,
            "无法连接到包注册表：连接被拒绝。",
            "检查网络连接或系统代理设置后重试。",
        ));
        assert!(matches!(published, ChannelProbe::Published(_)));
        assert!(matches!(unpublished, ChannelProbe::Unpublished));
        assert!(matches!(failed, ChannelProbe::Failed(_)));
    }

    #[test]
    fn package_release_carries_the_five_frozen_fields() {
        let r = release();
        assert_eq!(r.version.to_string(), "0.2.0-rc.2");
        assert_eq!(r.published_at_iso, "2026-10-02T08:11:32.114Z");
        assert!(r.integrity.as_deref().unwrap().starts_with("sha512-"));
        assert!(r.tarball_url.starts_with("https://"));
        assert_eq!(r.unpacked_size, Some(91756));
        assert_eq!(r.space_required(Some(1024)), 92780);
        assert_eq!(r.space_required(None), 91756);
        assert!(r.is_https());
    }
}

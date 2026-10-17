//! npm 注册表 HTTPS 适配器（主计划 §3.8 D-1）。
//!
//! 一次 GET 取完整 packument：三条通道一次请求拿全，同时拿到发布日期与 dist.integrity。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::{AppError, CmdResult};
use crate::m0::ProgressSink;
use crate::m1::channels::PACKAGE_NAME;
use crate::ports::registry::{ChannelProbe, PackageRelease, RegistrySource};

/// 注册表返回体。字段名保持 npm 原样（kebab-case 的 dist-tags）。
#[derive(Debug, Deserialize)]
pub struct Packument {
    #[serde(rename = "dist-tags")]
    pub dist_tags: BTreeMap<String, String>,
    pub versions: BTreeMap<String, VersionEntry>,
    /// npm 的时间映射：既有 created/modified，也有逐版本的发布日期
    #[serde(default)]
    pub time: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct VersionEntry {
    pub version: String,
    pub dist: Dist,
}

#[derive(Debug, Deserialize)]
pub struct Dist {
    pub tarball: String,
    /// SRI 字符串，实测形如 "sha512-<base64>"；缺失时必须拒绝安装（§3.8 D-2）
    #[serde(default)]
    pub integrity: Option<String>,
    #[serde(rename = "unpackedSize", default)]
    pub unpacked_size: Option<u64>,
}

/// packument 结构性问题。全部映射为 registry.parse，不做任何猜测。
#[derive(Debug)]
pub enum PackumentError {
    /// dist-tags 里指向的版本在 versions 里没有条目
    MissingVersionEntry { tag: String, version: String },
    /// versions[v].version 无法解析为 semver
    BadVersion { version: String },
    /// 该版本没有发布日期
    MissingPublishTime { version: String },
}

impl PackumentError {
    pub fn reason_zh(&self) -> String {
        match self {
            Self::MissingVersionEntry { tag, version } => format!(
                "包注册表返回的数据不完整：标签 {tag} 指向版本 {version}，但版本清单里没有这一条。"
            ),
            Self::BadVersion { version } => {
                format!("包注册表返回的版本号无法解析：{version}。")
            }
            Self::MissingPublishTime { version } => {
                format!("包注册表返回的版本 {version} 缺少发布日期。")
            }
        }
    }
}

/// 把注册表返回体映射为三态。
///
/// 规则（§3.8 D-1）：标签在 dist-tags 中 → Published；标签不在 → Unpublished；
/// 结构性问题 → Failed(registry.parse)。
pub fn map_packument(p: &Packument, dist_tag: &str) -> ChannelProbe {
    let Some(version_str) = p.dist_tags.get(dist_tag) else {
        return ChannelProbe::Unpublished;
    };
    let Some(entry) = p.versions.get(version_str) else {
        return parse_failed(PackumentError::MissingVersionEntry {
            tag: dist_tag.to_string(),
            version: version_str.clone(),
        });
    };
    let Ok(version) = semver::Version::parse(&entry.version) else {
        return parse_failed(PackumentError::BadVersion {
            version: entry.version.clone(),
        });
    };
    let Some(published) = p.time.get(version_str) else {
        return parse_failed(PackumentError::MissingPublishTime {
            version: version_str.clone(),
        });
    };
    ChannelProbe::Published(PackageRelease {
        version,
        published_at_iso: published.clone(),
        integrity: entry.dist.integrity.clone(),
        tarball_url: entry.dist.tarball.clone(),
        unpacked_size: entry.dist.unpacked_size,
    })
}

fn parse_failed(e: PackumentError) -> ChannelProbe {
    ChannelProbe::Failed(
        AppError::new(
            crate::error::REGISTRY_PARSE,
            e.reason_zh(),
            "稍后重试；若持续失败，请复制原始错误反馈。",
        )
        .retryable(true),
    )
}

/// 适配器。基址可注入（测试用夹具服务器），默认走真实注册表。
pub struct NpmRegistry {
    base_url: String,
    client: reqwest::Client,
    /// 单次请求超时；默认 10 秒，构造时可注入（异步测试要能缩短它）
    timeout: Duration,
}

impl NpmRegistry {
    pub const PROD_BASE_URL: &'static str = "https://registry.npmjs.org";

    /// 生产装配用：共享 m0::net 的 Client（保留 system-proxy，§1.3 红线 1/2）。
    pub fn from_net() -> Self {
        Self {
            base_url: Self::PROD_BASE_URL.to_string(),
            client: crate::m0::net::client().clone(),
            timeout: Duration::from_secs(10),
        }
    }

    /// 测试与注入用。
    pub fn with_base_url(
        base_url: impl Into<String>,
        client: reqwest::Client,
        timeout: Duration,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            client,
            timeout,
        }
    }

    fn packument_url(&self) -> String {
        // 作用域包名的 / 在 URL 里必须保留：registry.npmjs.org/@scope/name
        format!("{}/{PACKAGE_NAME}", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl RegistrySource for NpmRegistry {
    async fn probe(&self, channel: &str) -> ChannelProbe {
        let Some(def) = crate::m1::channels::by_id(channel) else {
            return ChannelProbe::Failed(
                AppError::new(
                    crate::error::REGISTRY_PARSE,
                    format!("请求了未定义的通道：{channel}。"),
                    "请从界面提供的通道列表中选择。",
                )
                .retryable(false),
            );
        };
        let url = self.packument_url();
        let request = async {
            let resp = self
                .client
                .get(&url)
                .header("accept", "application/json")
                .send()
                .await?;
            let status = resp.status();
            let body = resp.text().await?;
            Ok::<(reqwest::StatusCode, String), reqwest::Error>((status, body))
        };
        let (status, body) = match tokio::time::timeout(self.timeout, request).await {
            Err(_elapsed) => {
                return ChannelProbe::Failed(
                    AppError::new(
                        crate::error::NETWORK_OFFLINE,
                        format!(
                            "请求包注册表超时（超过 {} 秒）：{url}。",
                            self.timeout.as_secs()
                        ),
                        "检查网络连接或系统代理设置后重试。",
                    )
                    .retryable(true),
                );
            }
            Ok(Err(e)) => return ChannelProbe::Failed(crate::m0::net::classify(e)),
            Ok(Ok(v)) => v,
        };
        // 404 表示该包在注册表里没有发布记录 → 「尚未发布」，是正常返回值而非错误
        // （主计划 §3.2：registry.http 只覆盖非 404 状态；T-S6 把 404 留给适配器判定）。
        if status == reqwest::StatusCode::NOT_FOUND {
            return ChannelProbe::Unpublished;
        }
        if !status.is_success() {
            return ChannelProbe::Failed(
                AppError::new(
                    crate::error::REGISTRY_HTTP,
                    format!("包注册表返回 HTTP {}：{url}。", status.as_u16()),
                    "稍后重试；若使用代理，请检查代理是否可用。",
                )
                .retryable(true),
            );
        }
        match serde_json::from_str::<Packument>(&body) {
            Ok(p) => map_packument(&p, def.dist_tag),
            Err(e) => ChannelProbe::Failed(
                AppError::new(
                    crate::error::REGISTRY_PARSE,
                    format!("包注册表返回体无法解析：{e}。"),
                    "稍后重试；若持续失败，请复制原始错误反馈。",
                )
                .retryable(true)
                .with_raw(e.to_string()),
            ),
        }
    }

    /// 下载的唯一实现路径（§3.3：installer 不得绕过本方法直接出网）。
    ///
    /// 逐块写盘：每块之后检查取消、并 `progress.report(已下载字节, 总字节)`。
    /// 只有原始字节数——百分比与剩余时间由 M1 的 installer 换算（§3.3.1 分层约束）。
    async fn download(
        &self,
        release: &PackageRelease,
        dest: &Path,
        cancel: crate::m0::cancel::CancelToken,
        progress: ProgressSink,
    ) -> CmdResult<()> {
        use std::io::Write as _;

        // 已取消的令牌在出网前就直接返回：不建目录、不建文件、也不发请求。
        if cancel.is_cancelled() {
            return Err(AppError::new(
                crate::error::DOWNLOAD_CANCELLED,
                "下载已被取消。",
                "需要时重新点击「安装」即可。",
            )
            .retryable(true));
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::new(
                    crate::error::PATH_PERMISSION_DENIED,
                    format!("无法创建临时目录 {}：{e}", parent.display()),
                    "检查该目录的写入权限后重试。",
                )
            })?;
        }
        let resp = self
            .client
            .get(&release.tarball_url)
            .send()
            .await
            .map_err(crate::m0::net::classify)?;
        if !resp.status().is_success() {
            return Err(AppError::new(
                crate::error::REGISTRY_HTTP,
                format!("下载安装包失败：HTTP {}。", resp.status().as_u16()),
                "稍后重试；若使用代理，请检查代理是否可用。",
            )
            .retryable(true));
        }
        let total = resp.content_length();
        let mut file = std::fs::File::create(dest).map_err(|e| {
            AppError::new(
                crate::error::PATH_PERMISSION_DENIED,
                format!("无法写入 {}：{e}", dest.display()),
                "检查磁盘剩余空间与目录权限后重试。",
            )
        })?;
        let mut stream = resp.bytes_stream();
        let mut done: u64 = 0;
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
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
            let bytes = chunk.map_err(crate::m0::net::classify)?;
            file.write_all(&bytes).map_err(|e| {
                AppError::new(
                    crate::error::PATH_PERMISSION_DENIED,
                    format!("写入 {} 失败：{e}", dest.display()),
                    "检查磁盘剩余空间与目录权限后重试。",
                )
            })?;
            done += bytes.len() as u64;
            progress.report(done, total);
        }
        file.flush().map_err(|e| {
            AppError::new(
                crate::error::PATH_PERMISSION_DENIED,
                format!("刷新 {} 失败：{e}", dest.display()),
                "检查磁盘剩余空间后重试。",
            )
        })?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "fake/fake_registry.rs"]
mod fake_registry;
#[cfg(test)]
#[path = "fake/fixture_server.rs"]
mod fixture_server;

#[cfg(test)]
mod tests {
    use super::*;
    use fake_registry::{FakeProbe, FakeRegistry};
    use fixture_server::FixtureServer;

    /// 与真实注册表同形的最小 packument 夹具（版本清单取自 2026-10-16 实测）。
    const REAL_PACKUMENT: &str = r#"{
      "name": "@deepseek-ai/dsh",
      "dist-tags": { "latest": "0.2.0-rc.2", "next": "0.2.0-rc.2", "alpha": "0.2.1-alpha.4" },
      "time": {
        "created": "2026-09-01T00:00:00.000Z",
        "modified": "2026-10-16T00:00:00.000Z",
        "0.1.0-rc.6": "2026-09-20T10:00:00.000Z",
        "0.1.0-rc.7": "2026-09-22T10:00:00.000Z",
        "0.1.0-rc.8": "2026-09-24T10:00:00.000Z",
        "0.2.0-rc.1": "2026-09-28T10:00:00.000Z",
        "0.2.0-rc.2": "2026-10-02T08:11:32.114Z",
        "0.2.1-alpha.4": "2026-10-14T09:00:00.000Z"
      },
      "versions": {
        "0.1.0-rc.8": { "version": "0.1.0-rc.8", "dist": {
          "tarball": "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.1.0-rc.8.tgz",
          "integrity": "sha512-OLD=", "unpackedSize": 80000 } },
        "0.2.0-rc.2": { "version": "0.2.0-rc.2", "dist": {
          "tarball": "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.0-rc.2.tgz",
          "fileCount": 19,
          "integrity": "sha512-M/fYF6HLIlynRN+CGKapkKYX/pWzFLoZEARWYup+30ngo9e3yhAZGWFl7YLAgG0Yr/BuP55T4Z4hBMWkae4B1w==",
          "unpackedSize": 91756 } },
        "0.2.1-alpha.4": { "version": "0.2.1-alpha.4", "dist": {
          "tarball": "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.1-alpha.4.tgz",
          "integrity": "sha512-ALPHA=", "unpackedSize": 93000 } }
      }
    }"#;

    fn parse(s: &str) -> Packument {
        serde_json::from_str(s).expect("夹具 JSON 必须能解析")
    }

    /// 下载用例的版本记录：只有 tarball 地址随夹具服务器变。
    fn release_for(tarball_url: &str) -> PackageRelease {
        PackageRelease {
            version: semver::Version::parse("0.2.0-rc.2").unwrap(),
            published_at_iso: "2026-10-02T08:11:32.114Z".to_string(),
            integrity: Some("sha512-abc".to_string()),
            tarball_url: tarball_url.to_string(),
            unpacked_size: Some(91756),
        }
    }

    #[test]
    fn published_tag_yields_version_time_integrity_and_tarball() {
        let p = parse(REAL_PACKUMENT);
        let ChannelProbe::Published(r) = map_packument(&p, "latest") else {
            panic!("latest 必须映射为 Published");
        };
        assert_eq!(r.version.to_string(), "0.2.0-rc.2");
        assert_eq!(r.published_at_iso, "2026-10-02T08:11:32.114Z");
        assert_eq!(r.unpacked_size, Some(91756));
        assert!(r.integrity.as_deref().unwrap().starts_with("sha512-"));
        assert_eq!(
            r.tarball_url,
            "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.0-rc.2.tgz"
        );
        assert!(r.is_https());
    }

    #[test]
    fn alpha_channel_maps_to_its_own_dist_tag() {
        let p = parse(REAL_PACKUMENT);
        let ChannelProbe::Published(r) = map_packument(&p, "alpha") else {
            panic!("alpha 必须映射为 Published");
        };
        assert_eq!(r.version.to_string(), "0.2.1-alpha.4");
    }

    #[test]
    fn missing_tag_is_unpublished_not_an_error() {
        let p = parse(REAL_PACKUMENT);
        assert!(matches!(
            map_packument(&p, "beta"),
            ChannelProbe::Unpublished
        ));
    }

    #[test]
    fn empty_dist_tags_is_unpublished() {
        let p = parse(r#"{ "dist-tags": {}, "versions": {}, "time": {} }"#);
        assert!(matches!(
            map_packument(&p, "latest"),
            ChannelProbe::Unpublished
        ));
    }

    #[test]
    fn tag_pointing_at_absent_version_entry_is_registry_parse_failure() {
        let p = parse(
            r#"{ "dist-tags": { "latest": "9.9.9" }, "versions": {},
                 "time": { "9.9.9": "2026-10-01T00:00:00.000Z" } }"#,
        );
        let ChannelProbe::Failed(e) = map_packument(&p, "latest") else {
            panic!("标签指向不存在的版本条目必须判为检查失败");
        };
        assert_eq!(e.code, crate::error::REGISTRY_PARSE);
        assert!(
            e.reason.contains("9.9.9"),
            "原因里要带上版本号，实际为 {}",
            e.reason
        );
    }

    #[test]
    fn unparseable_version_is_registry_parse_failure() {
        let p = parse(
            r#"{ "dist-tags": { "latest": "not-a-version" },
                 "versions": { "not-a-version": { "version": "not-a-version",
                   "dist": { "tarball": "https://example.invalid/x.tgz" } } },
                 "time": { "not-a-version": "2026-10-01T00:00:00.000Z" } }"#,
        );
        let ChannelProbe::Failed(e) = map_packument(&p, "latest") else {
            panic!("非法版本号必须判为检查失败，不得回落到默认值");
        };
        assert_eq!(e.code, crate::error::REGISTRY_PARSE);
    }

    #[test]
    fn missing_publish_time_is_registry_parse_failure() {
        let p = parse(
            r#"{ "dist-tags": { "latest": "0.2.0-rc.2" },
                 "versions": { "0.2.0-rc.2": { "version": "0.2.0-rc.2",
                   "dist": { "tarball": "https://example.invalid/x.tgz" } } },
                 "time": {} }"#,
        );
        let ChannelProbe::Failed(e) = map_packument(&p, "latest") else {
            panic!("缺发布日期必须判为检查失败");
        };
        assert!(e.reason.contains("发布日期"));
    }

    #[test]
    fn missing_integrity_and_unpacked_size_are_none_not_errors() {
        let p = parse(
            r#"{ "dist-tags": { "latest": "0.2.0-rc.2" },
                 "versions": { "0.2.0-rc.2": { "version": "0.2.0-rc.2",
                   "dist": { "tarball": "https://example.invalid/x.tgz" } } },
                 "time": { "0.2.0-rc.2": "2026-10-02T00:00:00.000Z" } }"#,
        );
        let ChannelProbe::Published(r) = map_packument(&p, "latest") else {
            panic!("字段缺失不是错误");
        };
        assert!(r.integrity.is_none());
        assert!(r.unpacked_size.is_none());
    }

    #[test]
    fn packument_without_time_or_versions_fails_to_deserialize() {
        // versions 是必需字段：缺字段的返回体必须在反序列化阶段就被拒，不得当作「未发布」
        assert!(
            serde_json::from_str::<Packument>(r#"{ "dist-tags": { "latest": "1.0.0" } }"#).is_err()
        );
    }

    #[tokio::test]
    async fn real_get_reads_packument_from_a_live_socket_and_maps_two_states() {
        let server =
            FixtureServer::start(200, "application/json", REAL_PACKUMENT.as_bytes().to_vec());
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let ChannelProbe::Published(r) = reg.probe("next").await else {
            panic!("夹具服务器返回的 packument 必须被解析为 Published");
        };
        assert_eq!(r.version.to_string(), "0.2.0-rc.2");
        // 夹具只服务一次连接：第二个状态用一个新的夹具实例。
        // 注意不能用未定义通道试「未发布」——by_id 会先拒绝它（那是选错通道，不是未发布）。
        let server = FixtureServer::start(
            200,
            "application/json",
            br#"{ "dist-tags": {}, "versions": {}, "time": {} }"#.to_vec(),
        );
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        assert!(matches!(
            reg.probe("alpha").await,
            ChannelProbe::Unpublished
        ));
    }

    #[tokio::test]
    async fn non_2xx_is_registry_http_failure() {
        let server = FixtureServer::start(500, "application/json", b"{}".to_vec());
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let ChannelProbe::Failed(e) = reg.probe("latest").await else {
            panic!("HTTP 500 必须判为检查失败");
        };
        assert_eq!(e.code, crate::error::REGISTRY_HTTP);
        assert!(e.retryable);
    }

    #[tokio::test]
    async fn unparsable_body_is_registry_parse_failure() {
        let server = FixtureServer::start(200, "application/json", b"<html>nope</html>".to_vec());
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let ChannelProbe::Failed(e) = reg.probe("latest").await else {
            panic!("非 JSON 响应体必须判为检查失败");
        };
        assert_eq!(e.code, crate::error::REGISTRY_PARSE);
    }

    #[tokio::test]
    async fn timeout_is_network_offline_and_retryable() {
        let server = FixtureServer::start_black_hole();
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_millis(300),
        );
        let ChannelProbe::Failed(e) = reg.probe("latest").await else {
            panic!("超时必须判为检查失败");
        };
        assert_eq!(e.code, crate::error::NETWORK_OFFLINE);
        assert!(e.retryable, "超时可重试");
        assert!(
            e.reason.contains("超时"),
            "原因要说明是超时，实际为 {}",
            e.reason
        );
    }

    #[tokio::test]
    async fn unknown_channel_is_rejected_without_a_request() {
        let server =
            FixtureServer::start(200, "application/json", REAL_PACKUMENT.as_bytes().to_vec());
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let ChannelProbe::Failed(e) = reg.probe("gamma").await else {
            panic!("未定义通道必须被拒绝");
        };
        assert_eq!(e.code, crate::error::REGISTRY_PARSE);
        assert!(!e.retryable, "选错通道重试没有意义");
    }

    #[tokio::test]
    async fn fake_registry_serves_all_three_states() {
        let rel = PackageRelease {
            version: semver::Version::parse("0.2.0-rc.2").unwrap(),
            published_at_iso: "2026-10-02T08:11:32.114Z".to_string(),
            integrity: Some("sha512-abc".to_string()),
            tarball_url: "https://registry.npmjs.org/@deepseek-ai/dsh/-/dsh-0.2.0-rc.2.tgz"
                .to_string(),
            unpacked_size: Some(91756),
        };
        let fake = FakeRegistry::new()
            .with_probe("latest", vec![FakeProbe::Published(rel)])
            .with_probe("next", vec![FakeProbe::Unpublished])
            .with_probe(
                "alpha",
                vec![FakeProbe::Failed(
                    crate::error::NETWORK_OFFLINE.to_string(),
                    "无法连接到包注册表：连接被拒绝。".to_string(),
                )],
            );
        assert!(matches!(
            fake.probe("latest").await,
            ChannelProbe::Published(_)
        ));
        assert!(matches!(
            fake.probe("next").await,
            ChannelProbe::Unpublished
        ));
        let ChannelProbe::Failed(e) = fake.probe("alpha").await else {
            panic!("注入的失败必须原样返回");
        };
        assert_eq!(e.code, crate::error::NETWORK_OFFLINE);
        assert_eq!(fake.probe_calls(), 3);
        assert_eq!(fake.log().len(), 3);
    }

    #[tokio::test]
    async fn not_found_is_unpublished_not_a_registry_http_error() {
        // 主计划 §3.2：404 表示「尚未发布」，必须作为正常返回值；不得产生 registry.http/AppError。
        let server = FixtureServer::start(404, "application/json", b"{}".to_vec());
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        assert!(matches!(
            reg.probe("latest").await,
            ChannelProbe::Unpublished
        ));
    }

    #[tokio::test]
    async fn download_writes_exact_bytes_and_reports_monotonic_progress_to_100_percent() {
        const BODY: &[u8] = b"tarball-bytes-\x00\x01\x02-payload";
        let server = FixtureServer::start(200, "application/octet-stream", BODY.to_vec());
        let dir = crate::m0::paths::temp_root("npm-download-ok");
        let dest = dir.join("pkg.tgz");
        let release = release_for(&format!("{}/pkg.tgz", server.base_url()));
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = {
            let seen = std::sync::Arc::clone(&seen);
            ProgressSink::new(move |downloaded, total| {
                seen.lock().unwrap().push((downloaded, total));
            })
        };
        reg.download(
            &release,
            &dest,
            crate::m0::cancel::CancelToken::new_for_test(),
            sink,
        )
        .await
        .expect("真实适配器下载必须成功");

        assert_eq!(
            std::fs::read(&dest).unwrap(),
            BODY,
            "落盘字节必须与响应体完全一致"
        );
        let reports = seen.lock().unwrap().clone();
        assert!(!reports.is_empty(), "至少要上报一次进度");
        let total = BODY.len() as u64;
        assert!(
            reports.iter().all(|(_, t)| *t == Some(total)),
            "总字节数必须始终是响应体长度：{reports:?}"
        );
        for pair in reports.windows(2) {
            assert!(pair[1].0 >= pair[0].0, "进度必须单调不减：{reports:?}");
        }
        assert_eq!(
            reports.last().unwrap().0,
            total,
            "进度必须终止于 100%：{reports:?}"
        );
    }

    #[tokio::test]
    async fn download_with_a_pre_cancelled_token_fails_with_download_cancelled_and_no_file() {
        let server =
            FixtureServer::start(200, "application/octet-stream", b"never-written".to_vec());
        let dir = crate::m0::paths::temp_root("npm-download-cancelled");
        let dest = dir.join("pkg.tgz");
        let release = release_for(&format!("{}/pkg.tgz", server.base_url()));
        let reg = NpmRegistry::with_base_url(
            server.base_url(),
            crate::m0::net::client().clone(),
            Duration::from_secs(5),
        );
        let token = crate::m0::cancel::CancelToken::new_for_test();
        token.cancel();

        let err = reg
            .download(&release, &dest, token, ProgressSink::new(|_, _| {}))
            .await
            .expect_err("已取消的下载必须失败");
        assert_eq!(err.code, crate::error::DOWNLOAD_CANCELLED);
        assert!(err.retryable, "取消后允许用户重新点击安装");
        assert!(!dest.exists(), "取消后不得留下目标文件");
    }
}

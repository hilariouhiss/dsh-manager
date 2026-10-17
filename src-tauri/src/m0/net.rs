//! 出网唯一出口（主计划 §1.3 红线 1、红线 2，§3.3；FR-1.8；设计文档 §4.5）。
//!
//! 两条硬约束，改动前先读主计划 §1.3：
//! 1. reqwest 必须保留默认特性 —— 默认特性里的 system-proxy 是 FR-1.8「沿用系统代理」的
//!    唯一实现来源，关掉默认特性会**静默**失去代理支持；
//! 2. 不得调用 ClientBuilder 的代理禁用方法（它会同时关掉系统代理），也不得自建代理解析，
//!    更不得读写代理配置或其中的凭据。上述两条由本文件的单测守卫。

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;

use crate::error::{self, AppError};

/// 单次请求的总超时（30s）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

static CLIENT: OnceLock<Client> = OnceLock::new();

/// 进程内唯一的 HTTP 客户端：连接池复用，沿用系统代理。
pub fn client() -> &'static Client {
    CLIENT.get_or_init(
        || match Client::builder().timeout(REQUEST_TIMEOUT).build() {
            Ok(client) => client,
            Err(e) => {
                log::error!(
                    "HTTP 客户端初始化失败，退回默认客户端（本次运行失去 30s 总超时）：{e}"
                );
                Client::new()
            }
        },
    )
}

/// 与 reqwest 解耦的失败事实。reqwest::Error 无法在测试里构造，因此分类逻辑只吃这个结构。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorFacts {
    pub is_timeout: bool,
    pub is_connect: bool,
    pub is_decode: bool,
    pub status: Option<u16>,
    pub raw: String,
}

impl ErrorFacts {
    /// 从真实错误里提取分类所需的事实（提取本身不分类，便于单测覆盖分类表）。
    pub fn from_reqwest(e: &reqwest::Error) -> Self {
        Self {
            is_timeout: e.is_timeout(),
            is_connect: e.is_connect(),
            is_decode: e.is_decode(),
            status: e.status().map(|status| status.as_u16()),
            raw: describe(e),
        }
    }
}

/// reqwest 的 Display 只有「error sending request for url (...)」，真正的原因在 source 链里
/// （如代理失败时的「tunnel error: …」）。AppError.raw 要求「不吞错」，所以把整条 cause 链拼上。
fn describe(e: &reqwest::Error) -> String {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// 出网失败的分类结果（主计划 §3.2 与 §8.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpFailure {
    /// 代理需要认证（HTTP 407，或连接阶段被代理拒绝）→ network.proxy_auth
    ProxyAuth,
    /// TLS 校验失败 → network.tls
    Tls,
    /// DNS / 连接失败或超时 → network.offline
    Offline,
    /// 响应体无法解析 → registry.parse
    RegistryParse,
    /// 非 2xx（404 需由调用方先判为「尚未发布」）→ registry.http
    RegistryHttp(u16),
}

/// **冻结的公开入口（主计划 §3.3.1）：所有出网失败都必须经它翻译，禁止就地构造 `AppError`。**
///
/// 它只做两件事：提取事实、查表。真正可单测的逻辑在 `classify_facts` 与 `to_app_error`，
/// 因为 `reqwest::Error` 无法在测试里构造。
pub fn classify(err: reqwest::Error) -> AppError {
    let facts = ErrorFacts::from_reqwest(&err);
    to_app_error(classify_facts(&facts), &facts.raw)
}

/// 分类：纯函数，逐条单测。判定顺序即优先级。
pub fn classify_facts(facts: &ErrorFacts) -> HttpFailure {
    let raw = facts.raw.to_ascii_lowercase();
    if facts.status == Some(407) {
        return HttpFailure::ProxyAuth;
    }
    if facts.is_connect && (raw.contains("proxy") || raw.contains("tunnel")) {
        return HttpFailure::ProxyAuth;
    }
    if facts.is_connect
        && (raw.contains("certificate") || raw.contains("tls") || raw.contains("handshake"))
    {
        return HttpFailure::Tls;
    }
    if let Some(status) = facts.status
        && !(200..300).contains(&status)
    {
        return HttpFailure::RegistryHttp(status);
    }
    if facts.is_decode {
        return HttpFailure::RegistryParse;
    }
    HttpFailure::Offline
}

/// 分类 → 统一错误结构（§3.2 错误码表）。文案一律「原因 + 下一步」（NFR-1）。
pub fn to_app_error(failure: HttpFailure, raw: &str) -> AppError {
    match failure {
        HttpFailure::ProxyAuth => AppError::new(
            error::NETWORK_PROXY_AUTH,
            "系统代理要求认证，本次请求未被放行",
            "在 Windows 的「设置 › 网络和 Internet › 代理」或 HTTPS_PROXY 环境变量中预先提供可用的代理凭据，然后重试；本工具不读取、不保存代理凭据。",
        )
        .with_raw(raw),
        HttpFailure::Tls => AppError::new(
            error::NETWORK_TLS,
            "无法建立加密连接：TLS 证书校验未通过",
            "检查系统时间是否正确、网络中间设备是否替换了证书，然后重试。",
        )
        .with_raw(raw),
        HttpFailure::Offline => AppError::new(
            error::NETWORK_OFFLINE,
            "网络请求未能完成：连接失败或超时",
            "检查网络连接与系统代理设置后重试。",
        )
        .with_raw(raw)
        .retryable(true),
        HttpFailure::RegistryParse => AppError::new(
            error::REGISTRY_PARSE,
            "npm 仓库返回的内容无法解析",
            "稍后重试；若反复出现，请导出诊断并反馈。",
        )
        .with_raw(raw)
        .retryable(true),
        HttpFailure::RegistryHttp(status) => AppError::new(
            error::REGISTRY_HTTP,
            format!("npm 仓库返回 HTTP {status}"),
            "稍后重试；若持续出现，请检查系统代理是否拦截了 registry.npmjs.org。",
        )
        .with_raw(raw)
        .retryable(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NET_SOURCE: &str = include_str!("net.rs");
    const CARGO_TOML: &str = include_str!("../../Cargo.toml");

    fn facts(
        is_timeout: bool,
        is_connect: bool,
        is_decode: bool,
        status: Option<u16>,
        raw: &str,
    ) -> ErrorFacts {
        ErrorFacts {
            is_timeout,
            is_connect,
            is_decode,
            status,
            raw: raw.to_owned(),
        }
    }

    #[test]
    fn classify_is_the_frozen_public_entry_point() {
        // 冻结签名：pub fn classify(err: reqwest::Error) -> AppError（主计划 §3.3.1）。
        // reqwest::Error 无法构造，因此这里用函数指针钉住它的形状。
        fn assert_signature(_: fn(reqwest::Error) -> AppError) {}
        assert_signature(classify);
    }

    #[test]
    fn request_timeout_is_thirty_seconds() {
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn client_is_a_singleton() {
        assert!(std::ptr::eq(client(), client()));
    }

    #[test]
    fn classify_maps_the_failure_table() {
        // ① 代理要求认证：HTTP 407
        assert_eq!(
            classify_facts(&facts(
                false,
                false,
                false,
                Some(407),
                "Proxy Authentication Required"
            )),
            HttpFailure::ProxyAuth
        );
        // ② 代理要求认证：连接阶段被代理拒绝
        assert_eq!(
            classify_facts(&facts(
                false,
                true,
                false,
                None,
                "error sending request: proxy rejected the CONNECT tunnel"
            )),
            HttpFailure::ProxyAuth
        );
        // ③ TLS 校验失败
        assert_eq!(
            classify_facts(&facts(
                false,
                true,
                false,
                None,
                "invalid peer certificate: UnknownIssuer"
            )),
            HttpFailure::Tls
        );
        // ④ 超时
        assert_eq!(
            classify_facts(&facts(true, false, false, None, "operation timed out")),
            HttpFailure::Offline
        );
        // ⑤ DNS / 连接失败
        assert_eq!(
            classify_facts(&facts(
                false,
                true,
                false,
                None,
                "dns error: failed to lookup address information"
            )),
            HttpFailure::Offline
        );
        // ⑥ 非 2xx
        assert_eq!(
            classify_facts(&facts(
                false,
                false,
                false,
                Some(500),
                "500 Internal Server Error"
            )),
            HttpFailure::RegistryHttp(500)
        );
        // ⑦ 响应体无法解析
        assert_eq!(
            classify_facts(&facts(
                false,
                false,
                true,
                Some(200),
                "error decoding response body"
            )),
            HttpFailure::RegistryParse
        );
        // ⑧ 404 表示发布标签不存在，由适配器判为「尚未发布」，不得在这里当成网络错误
        assert_eq!(
            classify_facts(&facts(false, false, false, Some(404), "404 Not Found")),
            HttpFailure::RegistryHttp(404)
        );
    }

    #[test]
    fn proxy_auth_is_not_retryable_and_tells_the_user_what_to_do() {
        let err = to_app_error(HttpFailure::ProxyAuth, "407 Proxy Authentication Required");
        assert_eq!(err.code, error::NETWORK_PROXY_AUTH);
        assert!(!err.retryable);
        assert!(err.reason.contains("代理"));
        assert!(err.next_step.contains("代理凭据"));
        assert_eq!(
            err.raw.as_deref(),
            Some("407 Proxy Authentication Required")
        );
    }

    #[test]
    fn tls_error_is_not_retryable() {
        let err = to_app_error(HttpFailure::Tls, "invalid peer certificate");
        assert_eq!(err.code, error::NETWORK_TLS);
        assert!(!err.retryable);
        assert!(!err.reason.is_empty());
        assert!(!err.next_step.is_empty());
    }

    #[test]
    fn offline_error_is_retryable() {
        let err = to_app_error(HttpFailure::Offline, "operation timed out");
        assert_eq!(err.code, error::NETWORK_OFFLINE);
        assert!(err.retryable);
    }

    #[test]
    fn registry_http_error_mentions_the_status_code() {
        let err = to_app_error(HttpFailure::RegistryHttp(503), "503 Service Unavailable");
        assert_eq!(err.code, error::REGISTRY_HTTP);
        assert!(err.reason.contains("503"));
        assert!(err.retryable);
    }

    #[test]
    fn registry_parse_error_is_retryable() {
        let err = to_app_error(HttpFailure::RegistryParse, "expected value at line 1");
        assert_eq!(err.code, error::REGISTRY_PARSE);
        assert!(err.retryable);
    }

    #[tokio::test]
    async fn classify_keeps_the_underlying_cause_so_an_unreachable_proxy_is_a_proxy_problem() {
        // 测试专用的显式代理：生产代码只用系统代理，这里只为确定性地制造一个
        // 「代理不可达」的真实 reqwest::Error（127.0.0.1:1 必然拒绝连接）。
        // 顶层 Display 只有「error sending request for url (...)」，
        // 真正的原因（tunnel error: …）在 source 链里，因此 raw 必须带上 cause 链。
        let client = Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").expect("测试代理地址合法"))
            .build()
            .expect("测试客户端可构造");
        let err = client
            .get("https://registry.npmjs.org/-/ping")
            .send()
            .await
            .expect_err("不可达代理必须失败");

        let classified = classify(err);
        assert_eq!(
            classified.code,
            error::NETWORK_PROXY_AUTH,
            "代理不可达必须报「代理问题」而不是「网络中断」：{classified:?}"
        );
        assert!(
            classified
                .raw
                .as_deref()
                .is_some_and(|raw| raw.contains("tunnel error")),
            "raw 必须保留底层原因：{:?}",
            classified.raw
        );
    }

    #[test]
    fn net_source_never_disables_the_system_proxy() {
        // 只看代码行：注释里出现方法名不算违规。
        let code: String = NET_SOURCE
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // 断言用的字符串必须在运行时拼出来：否则本测试自己的源码会被 include_str! 收录而误报。
        let needle = format!("{}{}", "no", "_proxy");
        assert!(
            !code.contains(&needle),
            "net.rs 不得调用 ClientBuilder 的代理禁用方法（主计划 §1.3 红线 2）"
        );
    }

    #[test]
    fn cargo_toml_keeps_reqwest_default_features() {
        let line = CARGO_TOML
            .lines()
            .find(|line| line.trim_start().starts_with("reqwest"))
            .expect("Cargo.toml 中必须有 reqwest 依赖");
        assert!(
            !line.contains("default-features"),
            "reqwest 必须保留默认特性（主计划 §1.3 红线 1）：{line}"
        );
    }
}

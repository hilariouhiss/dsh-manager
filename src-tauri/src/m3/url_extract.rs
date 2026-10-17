//! 从输出流里增量识别可访问地址（FR-3.3）。
//!
//! 逐行喂入；跨行拼接在提取器内部处理（设计文档 §5.3）；同一地址只上报一次。
//!
//! 跨行判定规则：只有当候选一直延伸到行尾、且权威段还没写完（`//` 之后既没有端口冒号、
//! 也没有路径斜杠）时才推迟到下一行；像 `http://127.0.0.1:3080` 这种行尾已完整的地址立即上报。

use std::collections::HashSet;

/// 明显不是「本机可访问地址」的噪声主机。配置化，便于按实测修正。
pub const NOISE_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "www.npmjs.com",
    "npmjs.com",
    "nodejs.org",
];

const SCHEMES: [&str; 2] = ["http://", "https://"];
/// 地址到此为止的字符。
const TERMINATORS: [char; 6] = [' ', '\t', '"', '\'', '<', '>'];
/// 需要从尾部剥掉的标点（中英文都算）。
const TRAILING_PUNCT: [char; 14] = [
    '.', ',', ';', '!', '?', ')', ']', '}', '。', '，', '、', '）', '】', '》',
];

/// 增量地址提取器。
#[derive(Debug, Default)]
pub struct UrlExtractor {
    seen: HashSet<String>,
    /// 上一行末尾那段「可能被截断」的地址，等下一行拼上来再判定。
    pending: String,
}

impl UrlExtractor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一行，返回本次**首次**识别到的地址。
    ///
    /// 返回值是 `Option<String>`，所以一行里出现多个地址时只报第一个；下一个会在后续
    /// `feed` 调用里被报出来（已报过的地址不会再报）。
    pub fn feed(&mut self, line: &str) -> Option<String> {
        let hay = if self.pending.is_empty() {
            line.to_string()
        } else {
            let mut joined = std::mem::take(&mut self.pending);
            joined.push_str(line);
            joined
        };

        let mut search_from = 0usize;
        while let Some(start) = find_scheme(&hay, search_from) {
            let rest = &hay[start..];
            let end = rest
                .find(|c| TERMINATORS.contains(&c))
                .unwrap_or(rest.len());
            let raw_candidate = &rest[..end];

            // 候选一直延伸到行尾、且权威段还没写完 → 可能是跨行拼接，留到下一行再判定。
            if end == rest.len() && is_incomplete_authority(raw_candidate) {
                self.pending = raw_candidate.to_string();
                return None;
            }

            let candidate = strip_trailing_punct(raw_candidate);
            if !is_noise(candidate) && self.seen.insert(candidate.to_string()) {
                return Some(candidate.to_string());
            }
            search_from = start + end.max(1);
        }
        None
    }
}

fn find_scheme(hay: &str, from: usize) -> Option<usize> {
    let lower = hay.to_ascii_lowercase();
    SCHEMES
        .iter()
        .filter_map(|s| lower[from..].find(s).map(|i| from + i))
        .min()
}

/// 权威段（`//` 之后到第一个 `/` 之前）还没写完：既没有端口冒号、也没有路径斜杠。
fn is_incomplete_authority(url: &str) -> bool {
    match url.split_once("//") {
        Some((_, rest)) => !rest.contains('/') && !rest.contains(':'),
        None => true,
    }
}

fn strip_trailing_punct(url: &str) -> &str {
    let has_bracket = url
        .split_once("//")
        .map(|(_, r)| r.contains('['))
        .unwrap_or(false);
    let mut end = url.len();
    while end > 0 {
        let ch = url[..end].chars().next_back().unwrap_or(' ');
        // IPv6 字面量结尾的 ] 是地址的一部分，不能剥。
        if ch == ']' && has_bracket {
            break;
        }
        if TRAILING_PUNCT.contains(&ch) {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    &url[..end]
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("//")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = match authority.strip_prefix('[') {
        Some(stripped) => stripped.split(']').next()?.to_string(),
        None => authority.split(':').next()?.to_string(),
    };
    Some(host.to_ascii_lowercase())
}

fn is_noise(url: &str) -> bool {
    match host_of(url) {
        Some(host) => {
            host.is_empty()
                || NOISE_HOSTS
                    .iter()
                    .any(|n| host == *n || host.ends_with(&format!(".{n}")))
        }
        // 连主机都解析不出来：不是可访问地址。
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_url_from_a_single_line() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("DSH Web UI listening on http://127.0.0.1:3080"),
            Some("http://127.0.0.1:3080".to_string())
        );
    }

    #[test]
    fn joins_a_url_split_across_two_lines() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("  Web UI: http://127.0.0.1"),
            None,
            "权威段还没写完，先别报"
        );
        assert_eq!(
            ex.feed(":3080/ (no-open)"),
            Some("http://127.0.0.1:3080/".to_string()),
            "跨行拼接在提取器内部完成"
        );
    }

    #[test]
    fn reports_the_same_url_only_once() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("open http://127.0.0.1:3080 now"),
            Some("http://127.0.0.1:3080".to_string())
        );
        assert_eq!(ex.feed("open http://127.0.0.1:3080 now"), None);
        assert_eq!(ex.feed("still http://127.0.0.1:3080 here"), None);
    }

    #[test]
    fn strips_trailing_punctuation() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("见 http://127.0.0.1:3080。"),
            Some("http://127.0.0.1:3080".to_string())
        );
        let mut ex2 = UrlExtractor::new();
        assert_eq!(
            ex2.feed("(http://localhost:1420)"),
            Some("http://localhost:1420".to_string())
        );
        let mut ex3 = UrlExtractor::new();
        assert_eq!(
            ex3.feed("http://localhost:1420, then"),
            Some("http://localhost:1420".to_string())
        );
    }

    #[test]
    fn ignores_obvious_noise_hosts() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("see https://registry.npmjs.org/@deepseek-ai/dsh"),
            None
        );
        assert_eq!(ex.feed("docs at https://www.npmjs.com/package/x"), None);
        // 噪声之后仍能识别真正的地址
        assert_eq!(
            ex.feed("registry https://registry.npmjs.org/x then http://127.0.0.1:3080"),
            Some("http://127.0.0.1:3080".to_string())
        );
    }

    #[test]
    fn handles_ipv6_and_non_loopback_hosts() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("on http://[::1]:3080/"),
            Some("http://[::1]:3080/".to_string())
        );

        let mut ex2 = UrlExtractor::new();
        assert_eq!(
            ex2.feed("on https://10.0.0.7:8443/ui"),
            Some("https://10.0.0.7:8443/ui".to_string()),
            "--host 指定的非 127.0.0.1 地址与 https 都要认（--tls-cert 会产出 https）"
        );
    }

    #[test]
    fn returns_none_when_there_is_no_url() {
        let mut ex = UrlExtractor::new();
        assert_eq!(ex.feed("starting up..."), None);
        assert_eq!(ex.feed(""), None);
        assert_eq!(ex.feed("port 3080 is free"), None);
    }

    #[test]
    fn a_line_with_two_urls_reports_the_first_only() {
        let mut ex = UrlExtractor::new();
        assert_eq!(
            ex.feed("a http://127.0.0.1:1 b http://127.0.0.1:2"),
            Some("http://127.0.0.1:1".to_string())
        );
        assert_eq!(
            ex.feed("a http://127.0.0.1:1 b http://127.0.0.1:2"),
            Some("http://127.0.0.1:2".to_string())
        );
    }
}

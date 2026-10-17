//! 一次请求用的本地 HTTP 夹具：用 std 的 TcpListener 顶替测试用的 mock 服务器。
//!
//! 存在的理由：真实验证「一次 GET 取 packument」与「超时映射」需要一条真实 TCP 链路，
//! 而 §1.3 红线 3 禁止为测试引入新依赖。

use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

pub struct FixtureServer {
    url: String,
    handle: Option<JoinHandle<()>>,
}

impl FixtureServer {
    /// 起一个服务器：接受一次连接，读掉请求，返回给定状态码与响应体。
    pub fn start(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("夹具服务器绑定失败");
        let addr = listener.local_addr().expect("读取夹具服务器地址失败");
        // 引用不能跨线程：先拷成 String 再移进闭包
        let content_type = content_type.to_string();
        let handle = std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            // 读到请求头结束为止，避免客户端在写响应前就收到 RST
            let mut buf = [0u8; 4096];
            let mut seen = Vec::new();
            loop {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        seen.extend_from_slice(&buf[..n]);
                        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let reason = if (200..300).contains(&status) {
                "OK"
            } else {
                "Error"
            };
            let head = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
            drop(stream);
        });
        Self {
            url: format!("http://{addr}"),
            handle: Some(handle),
        }
    }

    /// 起一个「接受连接但永不响应」的服务器，用来验证请求超时的映射。
    pub fn start_black_hole() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("夹具服务器绑定失败");
        let addr = listener.local_addr().expect("读取夹具服务器地址失败");
        let handle = std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            // 持住连接不写任何字节，直到测试结束
            std::thread::sleep(std::time::Duration::from_secs(30));
            drop(stream);
        });
        Self {
            url: format!("http://{addr}"),
            handle: Some(handle),
        }
    }

    /// 基址，形如 http://127.0.0.1:51234 —— 适配器基址注入点用它。
    pub fn base_url(&self) -> String {
        self.url.clone()
    }
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        // 不 join：黑洞服务器会睡 30 秒，让它随进程退出自然结束。
        self.handle.take();
    }
}

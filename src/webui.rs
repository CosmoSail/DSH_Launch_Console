//! 用系统默认浏览器打开 DSH Web UI。
//!
//! 这是 0.2.0 的核心变化：不再内置 WebView，DSH 的界面完全交给系统浏览器。
//! 新版 DSH 的 Web UI 需要 token 鉴权，因此首次打开时带上本次运行的 token
//! （服务端会换发签名 cookie 并 303 回 `/`），之后浏览器里已有 cookie，
//! 用干净地址打开即可。

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use crate::config;
use crate::trf;

/// 用系统默认浏览器打开 URL。
pub fn open_url(url: &str) -> Result<(), String> {
    config::log(&format!("opening in system browser: {}", url));
    #[cfg(windows)]
    {
        // 交给 ShellExecuteW（rundll32 亦可，但 ShellExecute 最直接）
        use std::process::Command;
        // cmd /C start 会弹黑框；改用 explorer.exe 转交，无窗口且复用默认浏览器
        let r = Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        r.map(|_| ()).map_err(|e| trf!("打开浏览器失败: {}", e))
    }
    #[cfg(not(windows))]
    {
        open::that(url).map_err(|e| trf!("打开浏览器失败: {}", e))
    }
}

/// 端口是否在监听（判断 DSH 是否已起来）。
pub fn listening(host: &str, port: u16) -> bool {
    let Ok(addr) = format!("{}:{}", host, port).parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// 构造要打开的地址。
///
/// `token` 为空时返回干净地址（浏览器里可能已有 cookie）。
pub fn build_url(base: &str, token: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    match token {
        Some(t) if !t.is_empty() => format!("{}/?token={}", base, t),
        _ => format!("{}/", base),
    }
}

/// 打开 DSH Web UI。
///
/// - 未启动（端口不通）：返回错误，由界面提示先启动
/// - 已启动：优先带 token 打开（保证首次也能直接进界面）
pub fn open_web_ui(base: &str, token: Option<&str>) -> Result<(), String> {
    let (host, port) = config::url_host_port(base);
    if !listening(&host, port) {
        return Err(trf!("DSH 尚未运行（{}:{} 无法连接）。请先点击「启动 DeepSeek Harness」。", host, port));
    }
    open_url(&build_url(base, token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_building() {
        let base = "http://127.0.0.1:3080";
        assert_eq!(build_url(base, Some("tok")), "http://127.0.0.1:3080/?token=tok");
        assert_eq!(build_url(base, None), "http://127.0.0.1:3080/");
        assert_eq!(build_url(base, Some("")), "http://127.0.0.1:3080/");
        // 末尾斜杠不应产生双斜杠
        assert_eq!(build_url("http://127.0.0.1:3080/", Some("t")), "http://127.0.0.1:3080/?token=t");
    }

    #[test]
    fn host_port_parsing() {
        assert_eq!(config::url_host_port("http://127.0.0.1:3080"), ("127.0.0.1".into(), 3080));
        assert_eq!(config::url_host_port("http://127.0.0.1:3090/"), ("127.0.0.1".into(), 3090));
        assert_eq!(config::url_host_port("http://localhost:4000/x"), ("localhost".into(), 4000));
    }
}

// dsh-launch-console — DSH Launch Console
//
// 一个只允许浏览 DSH 地址的微浏览器启动器（地址写死在下方常量里）。
// 同一套源码按平台编译出两个工具集版本：
//  - Windows 版：经 cmd + CREATE_NO_WINDOW 隐藏启动 Windows 版 DSH
//    （终端 + Web UI 一起启动），支持 Windows 原生工具链（read/write/
//    pwsh/grep/glob/web_search/subagent 等；POSIX-only 工具如 stock
//    bash/str_replace_editor 会报 DSH 的平台错误——那是 DSH 工具的实现
//    限制，浏览器无法改变）。
//  - Linux/darwin 版：经 sh 隐藏启动原生 DSH，DSH 全部 POSIX 工具可用。
//  1. 启动时用完全隐藏的方式执行 npx 启动 DSH（终端 + Web UI 一起启动）
//     - Windows: cmd + CREATE_NO_WINDOW（终端窗口自始不可见）
//     - Linux:   sh 直接 spawn（GUI 程序本无终端窗口）
//  2. 轮询 DSH 地址直到就绪，然后打开 WebView 窗口加载该地址
//  3. 浏览器拒绝导航到任何其他网址（导航处理器 + 新窗口全部拒绝 + 禁用开发者工具）；
//     外部 http(s) 链接改由系统默认浏览器打开（DSH 窗口本身保持锁定）
//  4. 单实例：同一时间只有一个窗口、一个托盘图标（Windows 命名互斥体 +
//     激活消息；Linux/macOS flock + SIGUSR1）——再次双击启动器只把已有
//     窗口拉回前台（隐藏到托盘时自动恢复）。托盘图标挂在自己的
//     message-only 消息窗口上（不依赖 tao 消息钩子）：左键恢复窗口，右键
//     菜单「设置 ▸ 直接关闭DSH Launch Console、最小化DSH Launch Console / 关闭」。
//     退出（托盘菜单「关闭」或关窗，✕ 行为可在设置里切换）时才结束由
//     本程序启动的隐藏 DSH 进程树：
//     - Windows: 命名作业对象 KILL_ON_JOB_CLOSE（进程句柄消失即内核清树，
//       崩溃/被杀也算）+ taskkill /T / TerminateProcess / Stop-Process /
//       netstat 兜底
//     - Linux/macOS: killpg（SIGTERM→SIGKILL）+ ss/lsof 端口兜底；
//       启动器崩溃时 DSH 残留，下次启动自动收养
//  5. 若 DSH 已在运行且无登记表（例如用户在终端里手动启动），只打开窗口，
//     退出时不杀它（附加模式）
#![cfg_attr(all(not(test), windows), windows_subsystem = "windows")]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
#[cfg(windows)]
use tao::event_loop::EventLoopProxy;
use tao::window::WindowBuilder;
use wry::{WebView, WebViewBuilder};

// ====================== 写死的配置（源码级） ======================
/// 浏览器唯一允许访问的地址（只认这一个地址及其子路径）
const DSH_URL: &str = "http://127.0.0.1:3080";
/// DSH 主机
const DSH_HOST: &str = "127.0.0.1";
/// DSH 端口
const DSH_PORT: u16 = 3080;
/// dsh 的 npm 包名（解析全局包 JS 入口用）。
const DSH_PACKAGE: &str = "@deepseek-ai/dsh";
/// 回退启动命令（解析不到全局 dsh 包入口时用；host/port/--no-open 同源注入）。
const NPX_COMMAND: &str = "npx -y @deepseek-ai/dsh web";
/// 等待 DSH 就绪的最长秒数
const WAIT_SECONDS: u64 = 180;
/// CREATE_NO_WINDOW：让被启动的终端窗口自始不可见（仅 Windows）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// =================================================================

// —— 可选环境变量覆盖（默认即上面写死的值；仅高级用户使用）——
// DSH_LAUNCH_CONSOLE_URL / DSH_LAUNCH_CONSOLE_NPX / DSH_LAUNCH_CONSOLE_WAIT / DSH_LAUNCH_CONSOLE_STATEDIR …
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}
/// 布尔开关（`DSH_LAUNCH_CONSOLE_*`）：只要设置了非空值即为真。
fn env_flag(key: &str) -> bool {
    !env_or(key, "").is_empty()
}
/// 取 OsString 覆盖值（`DSH_LAUNCH_CONSOLE_*`）。
fn env_os(key: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(key)
}

fn dsh_url() -> String { env_or("DSH_LAUNCH_CONSOLE_URL", DSH_URL) }

/// 从 DSH 地址里解析 host / port。启动参数（--host/--port）、端口探测、
/// 窗口导航三处**共用这一个来源**，避免"服务起了但探的不是同一个端口"。
fn url_host_port(url: &str) -> (String, u16) {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.trim().to_string(),
            p.trim().parse::<u16>().unwrap_or(DSH_PORT),
        ),
        None => (authority.trim().to_string(), DSH_PORT),
    }
}
fn dsh_port() -> u16 { url_host_port(&dsh_url()).1 }

/// dsh web 的启动参数（直连 node 时用）：显式绑 host/port，并阻止 dsh
/// 自己再弹系统默认浏览器。
fn web_args_for(url: &str) -> Vec<String> {
    let (host, port) = url_host_port(url);
    vec![
        "web".to_string(),
        "--host".to_string(),
        host,
        "--port".to_string(),
        port.to_string(),
        "--no-open".to_string(),
    ]
}
fn web_args() -> Vec<String> { web_args_for(&dsh_url()) }

/// 直连 node 时的完整参数：**入口路径在最前**，后面才是 dsh web 的参数。
/// （漏掉入口会让 node 把 `web` 当脚本名，报 "Cannot find module"。）
fn node_args_for(entry: &Path, url: &str) -> Vec<std::ffi::OsString> {
    let mut v: Vec<std::ffi::OsString> = vec![entry.as_os_str().to_os_string()];
    v.extend(web_args_for(url).into_iter().map(std::ffi::OsString::from));
    v
}
fn node_args(entry: &Path) -> Vec<std::ffi::OsString> { node_args_for(entry, &dsh_url()) }

/// 用户覆盖的启动命令（DSH_LAUNCH_CONSOLE_NPX）：设置了就完全按它执行，
/// 不再做任何包入口解析。
fn user_launch_command() -> Option<String> {
    let v = env_or("DSH_LAUNCH_CONSOLE_NPX", "").trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}
/// npx 回退命令（解析不到全局包时使用）。
fn npx_fallback_command() -> String {
    let (host, port) = url_host_port(&dsh_url());
    format!("{} --host {} --port {} --no-open", NPX_COMMAND, host, port)
}
fn wait_seconds() -> u64 { env_or("DSH_LAUNCH_CONSOLE_WAIT", &WAIT_SECONDS.to_string()).parse().unwrap_or(WAIT_SECONDS) }

fn state_dir() -> PathBuf {
    let overridden = env_or("DSH_LAUNCH_CONSOLE_STATEDIR", "");
    if !overridden.is_empty() {
        return PathBuf::from(overridden);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let dir = base.join("DSH-Launch-Console");
        dir
    }
    #[cfg(unix)]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let base = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        let dir = base.join("dsh-launch-console");
        dir
    }
}

/// WebView2 用户数据目录（cookie / 登录态都在这里）。
/// 固定放在状态目录下的 `webview2`，**不跟随 exe 文件名**——这样以后再改名、
/// 换安装目录都不会丢登录态（WebView2 的默认目录是 `<exe 名>.WebView2`）。
fn webview_data_dir() -> PathBuf {
    state_dir().join("webview2")
}

fn owner_file() -> PathBuf { state_dir().join("owner") }
fn server_log() -> PathBuf { std::env::temp_dir().join("DSH-Launch-Console-server.log") }
fn launcher_log() -> PathBuf { std::env::temp_dir().join("DSH-Launch-Console.log") }

// ---------- 日志（实时追加，带体积上限） ----------
/// 日志上限（字节，默认 1 MiB）：启动器日志超限时截头保留最近一半；
/// 会话日志（server/webview）超限时在下一次全新启动前轮转为 .old。
const LOG_MAX_BYTES: u64 = 1_048_576;

fn log_max_bytes() -> u64 {
    env_or("DSH_LAUNCH_CONSOLE_LOGMAX", &LOG_MAX_BYTES.to_string())
        .parse().unwrap_or(LOG_MAX_BYTES)
        .max(64 * 1024)
}

fn log(msg: &str) {
    let path = launcher_log();
    // 体积上限：写前检查，超限截头（保留最近一半，对齐行边界）
    if let Ok(meta) = fs::metadata(&path) {
        if meta.len() > log_max_bytes() {
            trim_log_head(&path, log_max_bytes());
        }
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", secs, msg);
    }
}

/// 截掉日志头部，保留最近约 cap/2 字节（从下一行行首开始，避免残行）。
fn trim_log_head(path: &Path, cap: u64) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= cap {
        return;
    }
    let Ok(bytes) = fs::read(path) else { return };
    if bytes.len() as u64 <= cap {
        return;
    }
    let keep = (cap / 2) as usize;
    let start = bytes.len().saturating_sub(keep);
    let cut = match bytes[start..].iter().position(|b| *b == b'\n') {
        Some(pos) => start + pos + 1,
        None => start,
    };
    let _ = fs::write(path, &bytes[cut..]);
    log(&format!("trimmed log {} to {} bytes", path.display(), bytes.len() - cut));
}

/// 单个会话日志超限时轮转为 .old（只保留上一代）。
fn rotate_if_big(path: &Path, cap: u64) {
    let Ok(meta) = fs::metadata(path) else { return };
    if meta.len() <= cap {
        return;
    }
    let old = PathBuf::from(format!("{}.old", path.display()));
    let _ = fs::remove_file(&old);
    let _ = fs::rename(path, &old);
    log(&format!("rotated log {} -> {} ({} bytes)", path.display(), old.display(), meta.len()));
}

/// 全新 DSH 会话开始前调用：把超限的会话日志轮转为 .old。
/// （会话运行期间日志文件被 cmd/DSH 持有，不能动；上限在下一次全新启动时生效。）
fn rotate_big_logs() {
    let cap = log_max_bytes();
    for path in [server_log(), std::env::temp_dir().join("DSH-Launch-Console-webview.log")] {
        rotate_if_big(&path, cap);
    }
}

// ---------- 新版 DSH 的 Web UI token 鉴权 ----------
/// 纯解析：从日志文本里提取最后一次 `dsh web: http://...?token=XXX` 的 token。
fn parse_token_from_log(text: &str) -> Option<String> {
    let mut token: Option<String> = None;
    for line in text.lines() {
        let Some(idx) = line.find("dsh web: ") else { continue };
        let rest = &line[idx + "dsh web: ".len()..];
        let url_part = rest.split_whitespace().next()?;
        if let Some(t) = url_part.split("?token=").nth(1) {
            let t = t.split(|c| c == '&' || c == '#').next().unwrap_or(t);
            if !t.is_empty() {
                token = Some(t.to_string());
            }
        }
    }
    token
}

/// 从服务日志读取**本次运行**的 token。日志是追加式的，历史运行会留下
/// 旧 token 行——只有本程序本次 spawn 之后写入的内容才属于当前 DSH，
/// 因此解析窗口以 spawn 时记录的字节偏移为起点（见 TOKEN_LOG_OFFSET）。
fn latest_web_token() -> Option<String> {
    let bytes = fs::read(server_log()).ok()?;
    token_from_log_after(&bytes, TOKEN_LOG_OFFSET.load(std::sync::atomic::Ordering::Relaxed) as usize)
}

/// 纯解析（含编码容错与偏移窗口）：offset 之后的字节流 → lossy UTF-8 → 提取 token。
fn token_from_log_after(bytes: &[u8], offset: usize) -> Option<String> {
    if offset >= bytes.len() {
        return None;
    }
    token_from_log_bytes(&bytes[offset..])
}

/// 纯解析（含编码容错）：任意字节流 → lossy UTF-8 → 提取 token。
fn token_from_log_bytes(bytes: &[u8]) -> Option<String> {
    parse_token_from_log(&String::from_utf8_lossy(bytes))
}

/// 本次运行 spawn 隐藏 DSH 前的服务日志字节偏移（旧 token 行都在这之前）。
static TOKEN_LOG_OFFSET: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

// ---------- 窗口初始页面（秒开方案） ----------
/// 窗口最初加载的内容。
enum InitialPage {
    Url(String), // 直接加载真实 DSH 页面（带 token 或热 cookie）
    Loading,     // 本地“启动中”占位页，token 到位后自动切换
}

/// 决定窗口最初加载什么：
/// - 任何模式下已就绪的有效 token（日志解析 / 剪贴板抄录）→ 直接带 token 秒开
/// - 附加模式 → 纯地址（有持久 cookie 直接进）
/// - owned 且无鉴权的旧版 DSH（预检 200）→ 纯地址直接进真实界面
/// - 其余 owned 情况 → 本地“启动中”页，后台拿到 token 后自动切换
fn choose_initial_page(owned: bool, token: Option<&str>, cookie_ok: Option<bool>) -> InitialPage {
    if let Some(t) = token {
        return InitialPage::Url(format!(
            "{}?token={}",
            dsh_url().trim_end_matches('/'),
            t
        ));
    }
    if !owned {
        return InitialPage::Url(dsh_url());
    }
    if cookie_ok == Some(true) {
        return InitialPage::Url(dsh_url());
    }
    InitialPage::Loading
}

/// 官方鲸鱼 logo：与 DSH WebUI 的 favicon.svg 同一份文件（编译期嵌入）。
/// 原文件为黑色鲸鱼 + `@media (prefers-color-scheme: dark){path{fill:#fff}}`，
/// 占位页是深色底，所以下面的 CSS 直接把它染成官方深色变体的白色（不受
/// 系统浅色主题影响，否则黑鲸鱼落在深色底上会看不见）。
const LOADING_ICON_SVG: &str = include_str!("../icon.svg");

/// 本地“启动中”占位页：窗口秒开、无网络依赖，token 到位后由 load_url 切换。
/// 带秒数计时，让冷启动的等待可见（DSH 自身冷启动可能要十几秒）。
/// 图标用官方鲸鱼 logo（内联 SVG，任意 DPI 都清晰）。
fn loading_html() -> String {
    const HEAD: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><style>
html,body{height:100%;margin:0;background:#16181d;display:flex;align-items:center;justify-content:center;font-family:system-ui,sans-serif;color:#7d8590;}
.box{text-align:center}
.icon{width:64px;height:64px;margin:0 auto 18px}
.icon svg{width:100%;height:100%;display:block}
.icon svg path{fill:#fff}
.t{font-size:15px;letter-spacing:.5px}
.s{font-size:12px;margin-top:8px;color:#5a6069}
</style></head><body><div class="box"><div class="icon">"#;
    const TAIL: &str = r#"</div><div class="t">DSH 启动中…</div><div class="s">已等待 <span id="n">0</span> 秒</div></div>
<script>var n=0;setInterval(function(){document.getElementById('n').textContent=++n;},1000);</script>
</body></html>"#;
    let mut html = String::with_capacity(HEAD.len() + LOADING_ICON_SVG.len() + TAIL.len());
    html.push_str(HEAD);
    html.push_str(LOADING_ICON_SVG);
    html.push_str(TAIL);
    html
}

/// 简单 HTTP GET 预检：返回任意地址的响应状态码。裸 TCP 实现，不引入 HTTP 依赖。
fn dsh_http_status_for(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (host_port, if url.starts_with("https") { 443 } else { 80 }),
    };
    let addr: SocketAddr = format!("{}:{}", host, port).parse().ok()?;
    use std::io::Read;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(800)).ok()?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host_port
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = [0u8; 1024];
    let n = stream.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let status_line = head.lines().next()?;
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// 预检 DSH 干净地址的状态码（用于识别“无鉴权的旧版 DSH”：200 = 无需 token）。
fn dsh_http_status() -> Option<u16> {
    dsh_http_status_for(&dsh_url())
}

/// 用真实 HTTP 请求验证一个候选 token 是否有效（2xx/3xx 视为有效——
/// 服务端对正确 token 回 303 + cookie，对错误 token 回 401）。这保证
/// 剪贴板里抄来的旧 token 绝不会被误用。
fn dsh_token_valid(token: &str) -> bool {
    // 注意必须有 `/`：裸解析器按 `host:port/路径` 切分，缺斜杠会把
    // “3199?token=...” 整个塞进端口字段导致解析失败。
    let url = format!("{}/?token={}", dsh_url().trim_end_matches('/'), token);
    matches!(dsh_http_status_for(&url), Some(s) if (200..400).contains(&s))
}

// ---------- 剪贴板“抄录”终端里的带 token 地址（Windows） ----------
/// 读取剪贴板文本（UTF-16）。
#[cfg(windows)]
fn clipboard_text() -> Option<String> {
    unsafe {
        use windows_sys::Win32::System::DataExchange::{
            CloseClipboard, GetClipboardData, OpenClipboard,
        };
        use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};
        use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let result = {
            let handle = GetClipboardData(CF_UNICODETEXT as u32);
            if handle.is_null() {
                None
            } else {
                let ptr = GlobalLock(handle) as *const u16;
                let text = if ptr.is_null() {
                    None
                } else {
                    let len = (0..4096).take_while(|&i| *ptr.add(i) != 0).count();
                    let slice = std::slice::from_raw_parts(ptr, len);
                    Some(String::from_utf16_lossy(slice))
                };
                let _ = GlobalUnlock(handle);
                text
            }
        };
        let _ = CloseClipboard();
        result
    }
}

/// 纯解析：从任意文本（终端里复制的那行/整段）提取匹配 base 地址的 token。
/// 接受 `dsh web: http://127.0.0.1:3080/?token=XXX` 或裸 URL 形式；
/// 地址不匹配当前 DSH 的一律拒绝。
fn token_from_url_text(text: &str, base: &str) -> Option<String> {
    let base_norm = base.trim_end_matches('/');
    for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '(' || c == ')' || c == '\r' || c == '\n') {
        if let Some(rest) = word.strip_prefix(base_norm) {
            let rest = rest.trim_start_matches('/');
            if let Some(t) = rest.strip_prefix("?token=") {
                let t = t.split(|c| c == '&' || c == '#').next().unwrap_or(t);
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// 剪贴板里的有效 token：文本匹配当前 DSH 地址 + 真实 HTTP 预检通过。
#[cfg(windows)]
fn clipboard_token_valid() -> Option<String> {
    let text = clipboard_text()?;
    let token = token_from_url_text(&text, &dsh_url())?;
    if dsh_token_valid(&token) {
        Some(token)
    } else {
        None
    }
}

#[cfg(not(windows))]
fn clipboard_token_valid() -> Option<String> {
    None
}

// ---------- 终端抄录（经典 conhost 控制台直接读取最后几行） ----------
/// 用 netstat 找到当前 DSH 端口的监听者 pid（容忍 GBK 输出的编码问题）。
#[cfg(windows)]
fn port_listener_pid() -> Option<u32> {
    let tmp = std::env::temp_dir().join("dsh-netstat.tmp");
    let _ = Command::new("cmd")
        .arg("/C")
        .arg(format!("netstat -ano > {}", tmp.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status();
    let bytes = fs::read(&tmp).ok()?;
    let _ = fs::remove_file(&tmp);
    let out = String::from_utf8_lossy(&bytes);
    let needle = format!(":{}", dsh_port());
    for line in out.lines() {
        if !line.contains(&needle) || !line.to_uppercase().contains("LISTENING") {
            continue;
        }
        if let Some(pid) = line.split_whitespace().last().and_then(|s| s.parse::<u32>().ok()) {
            if pid != std::process::id() {
                return Some(pid);
            }
        }
    }
    None
}

/// 取父进程 pid（Toolhelp 快照）。
#[cfg(windows)]
fn parent_pid(pid: u32) -> Option<u32> {
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == pid {
                    let parent = entry.th32ParentProcessID;
                    let _ = CloseHandle(snap);
                    return Some(parent);
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        None
    }
}

/// 尝试从运行 DSH 的终端里“抄录”最后几行并提取 `dsh web: ...?token=...`。
/// 只对经典 conhost 控制台有效；ConPTY（Windows Terminal 等）下
/// AttachConsole 虽成功但屏幕缓冲区为 0×0，返回 None，由剪贴板方案兜底。
#[cfg(windows)]
fn scrape_terminal_token() -> Option<String> {
    unsafe {
        use windows_sys::Win32::System::Console::{
            AttachConsole, FreeConsole, GetConsoleScreenBufferInfo, GetStdHandle,
            ReadConsoleOutputCharacterW, CONSOLE_SCREEN_BUFFER_INFO, COORD, STD_OUTPUT_HANDLE,
        };
        // 候选进程：DSH 节点 + 向上 5 代祖先（终端 cmd/宿主可能在其中）
        let mut candidates = Vec::new();
        let mut cur = port_listener_pid();
        for _ in 0..6 {
            let Some(pid) = cur else { break };
            candidates.push(pid);
            cur = parent_pid(pid);
        }
        for pid in candidates {
            let _ = FreeConsole();
            if AttachConsole(pid) == 0 {
                continue;
            }
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
            if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
                let _ = FreeConsole();
                continue;
            }
            let cols = info.dwSize.X as i32;
            let rows = info.dwSize.Y as i32;
            if cols <= 0 || rows <= 0 {
                // ConPTY 伪终端：缓冲区不可读
                let _ = FreeConsole();
                continue;
            }
            // 读最后 200 行（会话长时间运行后，token 行可能已滚出“最后几行”）
            let start = (rows - 200).max(0);
            let len = ((rows - start) * cols) as u32;
            let mut buf: Vec<u16> = vec![0; len as usize];
            let mut read: u32 = 0;
            let coord = COORD { X: 0, Y: start as i16 };
            let ok = ReadConsoleOutputCharacterW(handle, buf.as_mut_ptr(), len, coord, &mut read);
            let _ = FreeConsole();
            if ok == 0 {
                continue;
            }
            let text = String::from_utf16_lossy(&buf[..read as usize]);
            if let Some(t) = parse_token_from_log(&text) {
                return Some(t);
            }
        }
        None
    }
}

/// 汇集所有需要校验的外部 token 来源：终端抄录 → 剪贴板（按此顺序）。
#[cfg(windows)]
fn external_token_valid() -> Option<String> {
    scrape_terminal_token()
        .filter(|t| dsh_token_valid(t))
        .or_else(clipboard_token_valid)
}

#[cfg(not(windows))]
fn external_token_valid() -> Option<String> {
    None
}

/// token 来源优先级（惰性求值，“主力命中就不碰候补”，更快）：
/// - owned：① 服务日志解析（主——偏移窗口内即本次运行，可信且免 HTTP 校验）；
///   ② 终端抄录；③ 剪贴板（②③ 经真实 HTTP 校验后采用）。
/// - 附加模式：日志里的历史 token 一律不可信，只认终端抄录/剪贴板。
fn prioritize_token<L, E>(owned: bool, log_src: L, external_src: E) -> Option<String>
where
    L: FnOnce() -> Option<String>,
    E: FnOnce() -> Option<String>,
{
    if owned {
        log_src().or_else(external_src)
    } else {
        external_src()
    }
}

/// 当前已就绪的 token。
fn ready_token(owned: bool) -> Option<String> {
    prioritize_token(owned, || latest_web_token(), || external_token_valid())
}

// ---------- 端口探测 ----------
fn port_open(timeout: Duration) -> bool {
    let addr: SocketAddr = format!("{}:{}", DSH_HOST, dsh_port()).parse().unwrap_or_else(|_| {
        format!("{}:{}", DSH_HOST, DSH_PORT).parse().unwrap()
    });
    TcpStream::connect_timeout(&addr, timeout).is_ok()
}

// ---------- 进程存活 / 杀进程树 ----------
fn pid_alive_safe(pid: u32) -> bool {
    if pid == 0 { return false; }
    #[cfg(windows)]
    {
        // SAFETY: passing a u32 pid to Win32 process APIs is sound.
        unsafe { pid_alive_windows(pid) }
    }
    #[cfg(unix)]
    {
        pid_alive_unix(pid)
    }
}

#[cfg(windows)]
unsafe fn pid_alive_windows(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if handle.is_null() {
        return false;
    }
    let mut code: u32 = 0;
    let ok = GetExitCodeProcess(handle, &mut code) != 0;
    let _ = CloseHandle(handle);
    ok && code == STILL_ACTIVE as u32
}

#[cfg(unix)]
fn pid_alive_unix(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) performs only an existence probe.
    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    alive || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 平台化的“整棵树清理句柄”：
/// - Windows：KILL_ON_JOB_CLOSE 命名作业对象——每个窗口各持一个句柄，
///   内核按句柄数引用计数，最后一个句柄关闭时杀树（崩溃/被杀也算关闭）
/// - Linux：  进程组 id（spawn 时 process_group(0) 使其自成进程组）
enum KillHandle {
    #[cfg(windows)]
    Job(KillJob),
    #[cfg(unix)]
    Pgid(u32),
}

impl KillHandle {
    fn kill_all(&self) {
        match self {
            #[cfg(windows)]
            KillHandle::Job(job) => job.kill_all(),
            #[cfg(unix)]
            KillHandle::Pgid(pgid) => {
                // SAFETY: pgid comes from our own spawn(process_group(0)).
                unsafe { libc::killpg(*pgid as i32, libc::SIGTERM); }
                thread::sleep(Duration::from_millis(500));
                unsafe { libc::killpg(*pgid as i32, libc::SIGKILL); }
            }
        }
    }
}

/// Windows 命名作业对象名（每棵 DSH 进程树一个）：其他窗口凭它打开同一作业，
/// 各自持有一个句柄——内核按句柄数引用计数，最后一个句柄关闭时清整棵树。
#[cfg(windows)]
fn job_name_for(server_pid: u32) -> String {
    format!("DSH_Launch_Console_Job_{}", server_pid)
}

/// Windows 作业对象：把隐藏 DSH 进程树关进 KILL_ON_JOB_CLOSE 作业。
/// 命名作业对象可被其他窗口 OpenJobObjectW 打开：每多一个窗口就多一个句柄，
/// 内核按句柄数引用计数，最后一个句柄关闭（正常退出/崩溃/被杀）时自动清树。
#[cfg(windows)]
struct KillJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl KillJob {
    /// 匿名作业对象（老路径兜底 / 测试探针用）。
    fn create() -> Option<Self> {
        Self::create_internal(std::ptr::null())
    }

    /// 命名作业对象：供其他窗口打开加入引用计数。
    fn create_named(name: &str) -> Option<Self> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        Self::create_internal(wide.as_ptr())
    }

    fn create_internal(name: *const u16) -> Option<Self> {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            };
            let handle = CreateJobObjectW(std::ptr::null(), name);
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(handle);
                return None;
            }
            Some(Self { handle })
        }
    }

    /// 打开已有窗口创建的命名作业（本窗口的引用计数 +1）。
    fn open_named(name: &str) -> Option<Self> {
        unsafe {
            use windows_sys::Win32::System::JobObjects::OpenJobObjectW;
            // windows-sys 0.59 未导出该常量：STANDARD_RIGHTS_REQUIRED | 0x1F
            const JOB_OBJECT_ALL_ACCESS: u32 = 0x001F_001F;
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = OpenJobObjectW(JOB_OBJECT_ALL_ACCESS, 0, wide.as_ptr());
            if handle.is_null() {
                None
            } else {
                Some(Self { handle })
            }
        }
    }

    fn assign(&self, pid: u32) -> bool {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return false;
            }
            let ok = AssignProcessToJobObject(self.handle, process) != 0;
            CloseHandle(process);
            ok
        }
    }

    fn kill_all(&self) {
        unsafe {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for KillJob {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            CloseHandle(self.handle);
        }
    }
}

/// 单进程兜底杀（树的根进程）。
fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        // 1) taskkill /T /F —— 首选，整棵进程树一次清掉
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status();
        // 2) OpenProcess + TerminateProcess —— 兜底
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !handle.is_null() {
                let _ = TerminateProcess(handle, 1);
                let _ = CloseHandle(handle);
            }
        }
        // 3) PowerShell Stop-Process —— 第三道防线（5.1 与 7 都试一遍）
        for shell in ["powershell", "pwsh"] {
            let _ = Command::new(shell)
                .args(["-NoProfile", "-Command", &format!("Stop-Process -Id {} -Force -ErrorAction SilentlyContinue", pid)])
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .status();
        }
    }
    #[cfg(unix)]
    {
        // SIGTERM 然后 SIGKILL
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(500));
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

/// 若端口上仍有监听者（记录根进程已死但子进程占着端口），找到并清掉。
fn kill_port_holder() {
    #[cfg(windows)]
    {
        let tmp = std::env::temp_dir().join("dsh-netstat.tmp");
        let _ = Command::new("cmd")
            .arg("/C")
            .arg(format!("netstat -ano > {}", tmp.display()))
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status();
        if let Ok(bytes) = fs::read(&tmp) {
            let _ = fs::remove_file(&tmp);
            let out = String::from_utf8_lossy(&bytes); // netstat 表头可能是 GBK
            let needle = format!(":{}", dsh_port());
            for line in out.lines() {
                if !line.contains(&needle) || !line.to_uppercase().contains("LISTENING") {
                    continue;
                }
                if let Some(pid) = line.split_whitespace().last().and_then(|s| s.parse::<u32>().ok()) {
                    if pid != std::process::id() {
                        kill_tree(pid);
                    }
                }
            }
        }
        // 双保险：PowerShell 的 TCP 表
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-Command",
                &format!("Get-NetTCPConnection -LocalPort {} -State Listen -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique | ForEach-Object {{ Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }}", dsh_port())])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // ss -ltnp 优先，lsof 兜底
        let needle = format!(":{}", dsh_port());
        for tool in [("ss", vec!["-ltnp"]), ("lsof", vec!["-t", "-i", &format!(":{}", dsh_port())])] {
            let output = Command::new(tool.0)
                .args(&tool.1)
                .stdin(Stdio::null())
                .output();
            let Ok(output) = output else { continue };
            let text = String::from_utf8_lossy(&output.stdout);
            if tool.0 == "lsof" {
                for line in text.lines() {
                    if let Ok(pid) = line.trim().parse::<u32>() {
                        if pid != std::process::id() {
                            kill_tree(pid);
                        }
                    }
                }
                break;
            }
            let mut hit = false;
            for line in text.lines() {
                if !line.contains(&needle) {
                    continue;
                }
                hit = true;
                // 形如 "users:(("node",pid=1234,fd=23))"
                if let Some(start) = line.find("pid=") {
                    let rest = &line[start + 4..];
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(pid) = digits.parse::<u32>() {
                        if pid != std::process::id() {
                            kill_tree(pid);
                        }
                    }
                }
            }
            if hit {
                break;
            }
        }
    }
}

/// 关窗退出登记的结果：仍有其他窗口存活 / 我是最后一个。
enum Leave {
    NotLast(usize), // 剩余活窗口数
    Last,           // 我是最后一个，执行整树清理
}

/// 关窗退出登记：只有「最后一个窗口」才执行杀树。
/// 多个窗口共享同一棵 DSH 时，先关的窗口只把自己从成员表里移除、不杀；
/// 最后一个关闭的（无论是不是当初启动 DSH 的那个）负责清场。
fn leave_server(my_pid: u32, server_pid: u32, kill: Option<KillHandle>) {
    let outcome = with_lock(|| -> Leave {
        if let Some(mut reg) = read_registry() {
            if reg.server_pid == server_pid {
                prune_dead(&mut reg);
                reg.members.retain(|p| *p != my_pid);
                if reg.members.is_empty() {
                    return Leave::Last;
                }
                write_registry(&reg);
                return Leave::NotLast(reg.members.len());
            }
            // 登记表已被新会话接管（不匹配）：不再动它，只按“最后退出”
            // 处理自己记录的旧树（was_alive 闸门会防止误杀新树占的端口）。
        }
        Leave::Last
    });
    match outcome {
        Leave::NotLast(n) => {
            log(&format!(
                "window closed; {} other window(s) keep DSH alive (pid {})",
                n, server_pid
            ));
            drop(kill); // Windows: 释放本窗口的作业句柄（引用计数 -1），树由其余窗口保活
        }
        Leave::Last => finalize_kill(server_pid, kill),
    }
}

/// 最后一个窗口退出：真正执行杀树（整树句柄 → 单进程兜底链 → 端口兜底）。
/// 持锁清场，防止清理途中新窗口加入；登记表只在仍指向本树时删除。
/// 端口兜底仅在“我们记录的根进程在停止前仍存活”时启用——
/// 若根进程早已死亡、端口被别的程序占用，绝不误杀。
fn finalize_kill(server_pid: u32, kill: Option<KillHandle>) {
    let _lock = RegLock::acquire();
    // 清场前持锁再核对一次：若发现别的窗口已加入登记表（新窗口在
    // leave 与 finalize 之间抢到了锁），我们就不再是“最后一个”，
    // 让位给它们（它们关窗时自会清场）。
    if let Some(mut reg) = read_registry() {
        if reg.server_pid == server_pid {
            prune_dead(&mut reg);
            if !reg.members.is_empty() {
                log(&format!(
                    "another window joined during shutdown; deferring cleanup ({} window(s), pid {})",
                    reg.members.len(), server_pid
                ));
                drop(kill); // 释放本窗口的作业句柄，树由新加入的窗口保活
                return;
            }
        }
    }
    if server_pid > 0 {
        let was_alive = pid_alive_safe(server_pid);
        if let Some(handle) = kill {
            handle.kill_all();
            drop(handle); // Windows: 最后一个句柄关闭 -> 内核结束整个作业（KILL_ON_JOB_CLOSE）
        }
        kill_tree(server_pid);
        if was_alive {
            kill_port_holder();
        }
    }
    match read_registry() {
        Some(reg) if reg.server_pid == server_pid => {
            let _ = fs::remove_file(owner_file());
        }
        Some(_) => {} // 登记表已被新会话接管，不动
        None => {
            let _ = fs::remove_file(owner_file()); // 已不存在，补删无害
        }
    }
    log(&format!("hidden DSH process tree stopped (pid {})", server_pid));
}

// ---------- 窗口成员登记表（owner 文件，兼容旧版两种格式） ----------
/// v3: server_pid|job_name|member1,member2,...
/// v2（旧 Rust 版）: launcher_pid|server_pid
/// v1（旧 Electron 版）: {"launcherPid":..,"serverPid":..}
#[derive(Debug, Clone)]
struct Registry {
    server_pid: u32,   // 隐藏启动器根进程（cmd/sh）的 pid
    job_name: String,  // Windows 命名作业对象名；空串 = 未知/不可用
    members: Vec<u32>, // 所有登记窗口进程的 pid（含启动者）
}

fn parse_registry(text: &str) -> Option<Registry> {
    let text = text.trim();
    if text.starts_with('{') {
        // 旧 Electron 版写的 JSON
        let grab = |key: &str| -> Option<u32> {
            let pos = text.find(key)? + key.len();
            let rest = &text[pos..].trim_start_matches(|c| c == ':' || c == ' ' || c == '"');
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        };
        return Some(Registry {
            server_pid: grab("serverPid")?,
            job_name: String::new(),
            members: vec![grab("launcherPid")?],
        });
    }
    let parts: Vec<&str> = text.split('|').map(str::trim).collect();
    match parts.len() {
        2 => Some(Registry {
            server_pid: parts[1].parse().ok()?,
            job_name: String::new(),
            members: vec![parts[0].parse().ok()?],
        }),
        n if n >= 3 => {
            let server_pid = parts[0].parse().ok()?;
            let job_name = parts[1].to_string();
            let mut members = Vec::new();
            for p in parts[2..].join(",").split(',') {
                if let Ok(m) = p.trim().parse::<u32>() {
                    members.push(m);
                }
            }
            Some(Registry { server_pid, job_name, members })
        }
        _ => None,
    }
}

fn registry_text(reg: &Registry) -> String {
    let ms: Vec<String> = reg.members.iter().map(|m| m.to_string()).collect();
    format!("{}|{}|{}", reg.server_pid, reg.job_name, ms.join(","))
}

fn read_registry() -> Option<Registry> {
    fs::read_to_string(owner_file()).ok().and_then(|t| parse_registry(&t))
}

fn write_registry(reg: &Registry) {
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(owner_file(), registry_text(reg));
}

/// 剔除已死亡窗口的登记（崩溃未走退出登记时由后续操作代劳）。
fn prune_dead(reg: &mut Registry) {
    reg.members.retain(|p| pid_alive_safe(*p));
}

// ---------- 登记表互斥（多窗口并发的读-改-写必须持锁） ----------
/// Windows: 会话级命名互斥体；Unix: 对 owner.lock 文件 flock。
struct RegLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(unix)]
    _file: std::fs::File,
}

#[cfg(windows)]
impl RegLock {
    fn acquire() -> Option<Self> {
        unsafe {
            use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
            let name: Vec<u16> = "DSH_Launch_Console_Registry_Mutex"
                .encode_utf16().chain(std::iter::once(0)).collect();
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return None;
            }
            // WAIT_OBJECT_0 / WAIT_ABANDONED 都算拿到；超时则无锁继续（尽力而为）
            let _ = WaitForSingleObject(handle, 5000);
            Some(Self { handle })
        }
    }
}

#[cfg(windows)]
impl Drop for RegLock {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::ReleaseMutex;
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(unix)]
impl RegLock {
    fn acquire() -> Option<Self> {
        use std::os::unix::io::AsRawFd;
        let _ = fs::create_dir_all(state_dir());
        let file = fs::OpenOptions::new()
            .create(true).write(true)
            .open(state_dir().join("owner.lock"))
            .ok()?;
        // 非阻塞重试最多约 5 秒，然后阻塞等（拿不到锁也要有结果）
        for _ in 0..100 {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Some(Self { _file: file });
            }
            thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        Some(Self { _file: file })
    }
}

#[cfg(unix)]
impl Drop for RegLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// 持锁执行登记表读-改-写；极端情况下拿不到锁则无锁执行（尽力而为）。
fn with_lock<T>(f: impl FnOnce() -> T) -> T {
    let _lock = RegLock::acquire();
    f()
}

// ---------- 错误弹窗 ----------
fn log_tail(path: &Path, lines: usize) -> String {
    fs::read(path).map(|b| {
        let s = String::from_utf8_lossy(&b).into_owned();
        s.lines().rev().take(lines).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    }).unwrap_or_default()
}

#[cfg(windows)]
fn show_error(title: &str, text: &str) {
    if env_flag("DSH_LAUNCH_CONSOLE_NOMSGBOX") {
        log(&format!("message box suppressed: {} | {}", title, text));
        return;
    }
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let c: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn show_error(title: &str, text: &str) {
    log(&format!("{}: {}", title, text));
    if env_flag("DSH_LAUNCH_CONSOLE_NOMSGBOX") {
        return;
    }
    // 有 zenity 就弹图形框，否则只写日志（避免引入 GTK 依赖）
    let _ = Command::new("zenity")
        .args(["--error", "--title", title, "--text", &format!("{}\n\n{}", title, text)])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "macos")]
fn show_error(title: &str, text: &str) {
    log(&format!("{}: {}", title, text));
    if env_flag("DSH_LAUNCH_CONSOLE_NOMSGBOX") {
        return;
    }
    // macOS 用 osascript 弹系统对话框，失败则只写日志
    let script = format!(
        "display dialog {:?} with title {:?} with icon stop buttons {{\"好\"}} default button 1",
        format!("{}\n\n{}", title, text),
        title
    );
    let _ = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status();
}

fn show_start_failure(why: &str) {
    let tail = log_tail(&server_log(), 14);
    show_error(
        "DSH 启动失败",
        &format!("{}\n\n日志文件: {}\n\n最近输出:\n{}", why, server_log().display(), tail),
    );
}

// ---------- 全局 dsh 包入口解析（纯文件系统，不启动任何子进程） ----------
/// 启动失败分类：弹窗给出可操作的原因，而不是笼统的"启动失败"。
#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchFailure {
    /// 找不到 node / npx
    NoRuntime,
    /// 找不到 dsh 包入口（附探查过的路径）
    NoEntry(Vec<String>),
    /// 端口被占用（EADDRINUSE）
    PortInUse,
    /// npm 网络/镜像问题
    Network,
    /// 权限问题
    Permission,
    /// profile 依赖缺失（ERR_MODULE_NOT_FOUND 等）
    Dependency,
    /// 其它提前退出（退出码）
    Exited(i32),
    /// 等待超时
    Timeout,
    /// 无法生成/执行启动脚本
    SpawnFailed,
}

impl LaunchFailure {
    /// 日志用短标签。
    fn label(&self) -> String {
        match self {
            LaunchFailure::NoRuntime => "no-runtime".into(),
            LaunchFailure::NoEntry(p) => format!("no-entry(probed {})", p.len()),
            LaunchFailure::PortInUse => "port-in-use".into(),
            LaunchFailure::Network => "npm-network".into(),
            LaunchFailure::Permission => "permission".into(),
            LaunchFailure::Dependency => "missing-dependency".into(),
            LaunchFailure::Exited(c) => format!("exited({})", c),
            LaunchFailure::Timeout => "timeout".into(),
            LaunchFailure::SpawnFailed => "spawn-failed".into(),
        }
    }

    /// 弹窗正文：一句话原因 + 可操作建议。
    fn advice(&self) -> String {
        match self {
            LaunchFailure::NoRuntime => "未找到 Node.js / npx。\n\
                 请先安装 Node.js（https://nodejs.org，建议 ≥ 18；dsh 本身就是 Node 应用），\n\
                 或设置 DSH_LAUNCH_CONSOLE_NPX 指定你自己的启动命令。"
                .to_string(),
            LaunchFailure::NoEntry(probes) => format!(
                "未找到全局 dsh 包的 JS 入口（已检查 PATH 上的 dsh 与下列位置）：\n  {}\n\n\
                 可执行 `npm i -g {}` 全局安装（之后启动无需 npx，最快），\n\
                 或设置 DSH_LAUNCH_CONSOLE_NPX 指定启动命令。",
                probes.join("\n  "),
                DSH_PACKAGE
            ),
            LaunchFailure::PortInUse => format!(
                "端口 {} 已被其它程序占用（dsh 报 EADDRINUSE）。\n\
                 请先释放该端口，或用 DSH_LAUNCH_CONSOLE_URL=http://127.0.0.1:<其它端口> 换端口。",
                dsh_port()
            ),
            LaunchFailure::Network => "npm 下载失败（网络/镜像不通）。\n\
                 检查网络或代理；也可先 `npm i -g @deepseek-ai/dsh` 装好再启动（免 npx 联网）。"
                .to_string(),
            LaunchFailure::Permission => "权限不足（写入 DSH_HOME / node_modules 或执行 node 被拒）。\n\
                 请检查 DSH_HOME 与 npm 全局目录的权限。"
                .to_string(),
            LaunchFailure::Dependency => "dsh 的 profile 依赖不完整（找不到包 / ERR_MODULE_NOT_FOUND）。\n\
                 预览版 dsh 常见：升级到稳定版后重试（npm i -g @deepseek-ai/dsh@latest）。"
                .to_string(),
            LaunchFailure::Exited(code) => format!("DSH 提前退出（退出码 {}）。", code),
            LaunchFailure::Timeout => format!("DSH 在 {} 秒内未就绪。", wait_seconds()),
            LaunchFailure::SpawnFailed => {
                "无法启动 dsh 进程（状态目录不可写或 spawn 失败）。".to_string()
            }
        }
    }
}

/// 从服务日志尾部识别已知失败签名（大小写不敏感；越具体的越先判）。
fn classify_log_tail(tail: &str) -> Option<LaunchFailure> {
    let low = tail.to_ascii_lowercase();
    let has = |p: &str| low.contains(p);
    if has("eaddrinuse") || has("address already in use") || has("only one usage of each socket address") {
        return Some(LaunchFailure::PortInUse);
    }
    if has("err_module_not_found") || has("cannot find module") || has("cannot find package") || has("module_not_found") {
        return Some(LaunchFailure::Dependency);
    }
    if has("eacces") || has("eperm") || has("access is denied") || has("permission denied") || has("operation not permitted") {
        return Some(LaunchFailure::Permission);
    }
    if has("etimedout")
        || has("err_socket_timeout")
        || has("eai_again")
        || has("enotfound")
        || has("econnrefused")
        || has("econnreset")
        || has("fetch failed")
        || has("npm error network")
        || has("registry.npmjs.org")
        || has("registry.npmmirror.com")
    {
        return Some(LaunchFailure::Network);
    }
    None
}

/// 从日志尾部取"第一条像样的错误线索"（弹窗里显示，便于对号入座）。
fn first_error_clue(tail: &str) -> Option<String> {
    tail.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .find(|l| {
            let low = l.to_ascii_lowercase();
            l.starts_with("npm error")
                || l.starts_with("npm ERR")
                || low.contains("error")
                || low.contains("cannot find")
                || low.contains("eaddrinuse")
                || low.contains("throw")
        })
        .map(|l| l.chars().take(300).collect())
}

// ---------- 包入口解析 ----------
/// 直连运行所需的（node，包入口）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct DshEntry {
    node: PathBuf,
    entry: PathBuf,
    /// 解析来源说明（写日志用）
    source: String,
}

const JS_EXTS: [&str; 3] = ["js", "cjs", "mjs"];

fn has_js_ext(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| JS_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(windows)]
    {
        p.is_file()
    }
}

/// 在给定 PATH 值里查找可执行文件（Windows 按 PATHEXT 顺序，Unix 要求可执行位）。
/// 纯函数式接口：PATH/PATHEXT 作为参数传入，便于单测。
fn which_in(path_var: &OsStr, pathext: &str, name: &str) -> Option<PathBuf> {
    let mut candidates: Vec<String> = Vec::new();
    #[cfg(windows)]
    {
        if Path::new(name).extension().is_some() {
            candidates.push(name.to_string());
        }
        let exts: Vec<&str> = pathext
            .split(';')
            .map(|e| e.trim())
            .filter(|e| !e.is_empty())
            .collect();
        let exts: Vec<&str> =
            if exts.is_empty() { vec![".COM", ".EXE", ".BAT", ".CMD"] } else { exts };
        for ext in exts {
            candidates.push(format!("{}{}", name, ext));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pathext;
        candidates.push(name.to_string());
    }
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for cand in &candidates {
            let p = dir.join(cand);
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    which_in(&path, &pathext, name)
}

/// 从 shim 文本里抽出指向包入口的 JS 路径。
/// npm 的 `dsh.cmd`：`"%_prog%"  "%dp0%\node_modules\@deepseek-ai\dsh\lib\bin.js" %*`
/// pnpm/自建 shim：`exec node "$basedir/../@deepseek-ai/dsh/lib/bin.js" "$@"`
fn find_entry_in_shim_text(text: &str, shim_dir: &Path) -> Option<PathBuf> {
    for quote in ['"', '\''] {
        let mut rest = text;
        while let Some(start) = rest.find(quote) {
            let after = &rest[start + 1..];
            let Some(end) = after.find(quote) else { break };
            let raw = &after[..end];
            rest = &after[end + 1..];
            let low = raw.to_ascii_lowercase();
            if !JS_EXTS.iter().any(|x| low.ends_with(&format!(".{}", x))) {
                continue;
            }
            let mut s = raw.to_string();
            // 变量替换：npm 的 %dp0%（=%~dp0）与 pnpm 的 $basedir 都指 shim 所在目录
            for var in ["%dp0%", "%DP0%", "%~dp0%", "$basedir", "${basedir}"] {
                s = s.replace(var, &shim_dir.display().to_string());
            }
            let cand = PathBuf::from(s.replace('\\', std::path::MAIN_SEPARATOR_STR));
            let cand = if cand.is_absolute() { cand } else { shim_dir.join(cand) };
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// 解析 shim 文件 → 包入口。Unix 上 npm 用符号链接，Windows 上是文本 shim。
fn entry_from_shim(shim: &Path) -> Option<PathBuf> {
    if let Ok(target) = fs::read_link(shim) {
        let base = shim.parent().unwrap_or_else(|| Path::new("."));
        let resolved = if target.is_absolute() { target } else { base.join(target) };
        let resolved = fs::canonicalize(&resolved).unwrap_or(resolved);
        if resolved.is_file() && has_js_ext(&resolved) {
            return Some(resolved);
        }
    }
    let text = fs::read_to_string(shim).ok()?;
    find_entry_in_shim_text(&text, shim.parent().unwrap_or_else(|| Path::new(".")))
}

/// 从 package.json 文本里取 bin 入口（字符串形式，或对象里的 dsh 键）。
fn bin_field_from_package_json(txt: &str) -> Option<String> {
    let idx = txt.find("\"bin\"")?;
    let after = &txt[idx + 5..];
    let rest = after.get(after.find(':')? + 1..)?.trim_start();
    let value = if let Some(stripped) = rest.strip_prefix('"') {
        &stripped[..stripped.find('"')?]
    } else if let Some(obj) = rest.strip_prefix('{') {
        let body = &obj[..obj.find('}')?];
        let key = body.find("\"dsh\"")?;
        let kv = &body[key + 5..];
        let v = kv.get(kv.find(':')? + 1..)?.trim_start().strip_prefix('"')?;
        &v[..v.find('"')?]
    } else {
        return None;
    };
    Some(value.replace("\\/", "/").replace("\\\\", "\\"))
}

/// 从一个包里找 JS 入口：先读 package.json 的 bin，再试常见入口文件名。
fn entry_from_package_dir(pkg: &Path) -> Option<PathBuf> {
    if !pkg.is_dir() {
        return None;
    }
    if let Ok(txt) = fs::read_to_string(pkg.join("package.json")) {
        if let Some(rel) = bin_field_from_package_json(&txt) {
            let cand = pkg.join(rel);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for rel in ["lib/bin.js", "bin.js", "dist/bin.js", "lib/cli.js", "index.js"] {
        let cand = pkg.join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 已知的全局安装位置（版本化目录会展开一层子目录）。
fn global_package_dirs() -> Vec<PathBuf> {
    let pkg_rel = Path::new("@deepseek-ai").join("dsh");
    let mut out: Vec<PathBuf> = Vec::new();
    {
        let mut push_root = |root: PathBuf| out.push(root.join(&pkg_rel));
        #[cfg(windows)]
        {
            if let Some(a) = std::env::var_os("APPDATA") {
                push_root(PathBuf::from(a).join("npm").join("node_modules"));
            }
            if let Some(p) = std::env::var_os("ProgramFiles") {
                push_root(PathBuf::from(p).join("nodejs").join("node_modules"));
            }
            let versioned = [
                std::env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("pnpm").join("global")),
                std::env::var_os("APPDATA").map(|v| PathBuf::from(v).join("nvm")),
                std::env::var_os("NVM_HOME").map(PathBuf::from),
            ];
            for base in versioned.into_iter().flatten() {
                if let Ok(rd) = fs::read_dir(&base) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_dir() {
                            push_root(p.join("node_modules"));
                            push_root(p);
                        }
                    }
                }
            }
        }
        #[cfg(unix)]
        {
            push_root(PathBuf::from("/usr/local/lib/node_modules"));
            push_root(PathBuf::from("/usr/lib/node_modules"));
            push_root(PathBuf::from("/opt/homebrew/lib/node_modules"));
            if let Some(h) = std::env::var_os("HOME") {
                let h = PathBuf::from(h);
                push_root(h.join(".bun").join("install").join("global").join("node_modules"));
                let versioned = [
                    h.join(".nvm").join("versions").join("node"),
                    h.join(".local").join("share").join("pnpm").join("global"),
                ];
                for base in versioned {
                    if let Ok(rd) = fs::read_dir(&base) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p.is_dir() {
                                push_root(p.join("node_modules"));
                                push_root(p);
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// 解析 node 可执行文件：DSH_LAUNCH_CONSOLE_NODE 覆盖 → shim 同目录 → PATH。
fn resolve_node(shim: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = env_os("DSH_LAUNCH_CONSOLE_NODE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(dir) = shim.and_then(|s| s.parent()) {
        for name in ["node.exe", "node"] {
            let cand = dir.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    which("node")
}

/// 解析全局 dsh 包入口。顺序：PATH 上的 dsh（内嵌入口/符号链接）→ 已知全局目录。
/// 全程只读文件系统，不启动任何子进程（省掉 `where dsh` 的 70~95ms）。
fn resolve_dsh_entry() -> Result<DshEntry, LaunchFailure> {
    let mut probes: Vec<String> = Vec::new();
    if let Some(shim) = which("dsh") {
        if let Some(entry) = entry_from_shim(&shim) {
            if let Some(node) = resolve_node(Some(&shim)) {
                return Ok(DshEntry {
                    node,
                    entry,
                    source: format!("PATH 上的 {}", shim.display()),
                });
            }
        }
        probes.push(format!("{}（PATH 上的 dsh，未解析出入口/未找到 node）", shim.display()));
    } else {
        probes.push("PATH 上没有 dsh（按 PATHEXT/可执行位查找）".to_string());
    }
    for pkg in global_package_dirs() {
        if !pkg.is_dir() {
            continue;
        }
        probes.push(pkg.display().to_string());
        if let Some(entry) = entry_from_package_dir(&pkg) {
            if let Some(node) = resolve_node(None) {
                return Ok(DshEntry {
                    node,
                    entry,
                    source: format!("全局包目录 {}", pkg.display()),
                });
            }
        }
    }
    if which("node").is_none() {
        return Err(LaunchFailure::NoRuntime);
    }
    Err(LaunchFailure::NoEntry(probes))
}

/// 启动方案：解析阶段决定，spawn 阶段执行。
#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchPlan {
    /// 直连：node <entry> web --host H --port P --no-open（无 shell 层）
    NodeEntry(DshEntry),
    /// 回退：经隐藏脚本执行 npx
    Npx(String),
    /// 用户覆盖命令（DSH_LAUNCH_CONSOLE_NPX）
    User(String),
}

impl LaunchPlan {
    fn describe(&self) -> String {
        match self {
            LaunchPlan::NodeEntry(e) => format!(
                "node {} {}   [{}]",
                e.entry.display(),
                web_args().join(" "),
                e.source
            ),
            LaunchPlan::Npx(c) => format!("{}   [npx 回退]", c),
            LaunchPlan::User(c) => format!("{}   [用户覆盖 DSH_LAUNCH_CONSOLE_NPX]", c),
        }
    }
}

/// 决定启动方案：用户覆盖 > 全局包入口直连 > npx 回退。
fn plan_launch() -> LaunchPlan {
    if let Some(cmd) = user_launch_command() {
        return LaunchPlan::User(cmd);
    }
    match resolve_dsh_entry() {
        Ok(entry) => LaunchPlan::NodeEntry(entry),
        Err(why) => {
            log(&format!("global dsh entry resolution failed: {}", why.label()));
            if let LaunchFailure::NoEntry(probes) = &why {
                log(&format!("probed: {}", probes.join(" | ")));
            }
            LaunchPlan::Npx(npx_fallback_command())
        }
    }
}

/// 我方的用户主目录（与旧脚本的 `cd` 行为一致）。
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }
}

/// 往会话日志追加一行（直连 node 时没有批处理帮我们写标记）。
fn append_server_log(msg: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(server_log()) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "===== [{}] {}", secs, msg);
    }
}

// ---------- 启动 DSH（隐藏终端） ----------
/// 已 spawn 但尚未监听的 DSH：窗口先显示「启动中」页，就绪/失败/超时
/// 的轮询搬到事件循环里做，不再阻塞在窗口创建之前（省掉 1~2 秒白等）。
struct PendingBoot {
    child: std::process::Child,
    pid: u32,
    deadline: Instant,
}

enum Startup {
    Ok {
        owned: bool,
        server_pid: Option<u32>,
        kill: Option<KillHandle>,
        pending: Option<PendingBoot>,
    },
    Failed,
}

/// 冷启动轮询一步的结果。
#[derive(Debug, PartialEq, Eq)]
enum BootPoll {
    /// 端口已就绪：pending 已置空，可以进入 token 轮询
    Ready,
    /// 还没就绪，继续等
    Continue,
    /// DSH 进程提前退出（退出码）
    Exited(i32),
    /// 超过等待上限
    Timeout,
}

/// 冷启动轮询一步：检查端口/进程退出/超时。就绪时把 pending 置空。
fn boot_poll(pending: &mut Option<PendingBoot>) -> BootPoll {
    let Some(p) = pending.as_mut() else {
        return BootPoll::Ready;
    };
    if port_open(Duration::from_millis(200)) {
        *pending = None;
        return BootPoll::Ready;
    }
    if let Ok(Some(status)) = p.child.try_wait() {
        return BootPoll::Exited(status.code().unwrap_or(-1));
    }
    if Instant::now() >= p.deadline {
        return BootPoll::Timeout;
    }
    BootPoll::Continue
}

/// 冷启动失败的统一收尾：超时时杀整树，并清掉本进程写下的登记表。
fn abandon_boot(pid: u32, kill: Option<KillHandle>, timed_out: bool) {
    if timed_out {
        if let Some(handle) = kill {
            handle.kill_all();
        }
        kill_tree(pid);
    }
    with_lock(|| {
        if read_registry().map(|r| r.server_pid == pid).unwrap_or(false) {
            let _ = fs::remove_file(owner_file());
        }
    });
}

/// Windows 启动批处理内容（纯函数，便于单测）。
/// Windows 回退启动脚本内容（纯函数，便于单测）。
/// 只在"解析不到全局包入口"或"用户覆盖启动命令"时才会用到：
/// 直连方案不写脚本、不经 cmd。
#[cfg(windows)]
fn batch_script(home: &Path, cmd: &str, log: &Path) -> String {
    format!(
        "@echo off\r\n\
         echo ===== %date% %time% starting >> \"{log}\"\r\n\
         cd /d \"{home}\"\r\n\
         echo [launch] running: {cmd}\r\n\
         call {cmd} >> \"{log}\" 2>&1\r\n\
         set _EC=%errorlevel%\r\n\
         echo %date% %time% exited with code %_EC% >> \"{log}\"\r\n\
         exit /b %_EC%\r\n",
        log = log.display(), home = home.display(), cmd = cmd
    )
}

/// Linux/macOS 启动脚本内容（纯函数，便于单测）。
#[cfg(unix)]
fn shell_script(home: &Path, cmd: &str, log: &Path) -> String {
    format!(
        "#!/bin/sh\ncd \"{}\"\necho \"[launch] running: {}\"\n{} >> \"{}\" 2>&1\n",
        home.display(), cmd, cmd, log.display()
    )
}

/// 隐藏启动 dsh。
/// - 直连方案：直接 spawn `node <entry> web --host … --port … --no-open`
///   （没有 cmd/sh 中间层，日志由我们重定向，省掉 shell 与 `where` 探测开销）；
/// - 回退/用户覆盖：写隐藏脚本由 cmd /C（Windows）或 sh（Unix）执行，保持旧行为。
#[cfg(windows)]
fn spawn_hidden_dsh(plan: &LaunchPlan) -> Result<(std::process::Child, Option<PathBuf>), LaunchFailure> {
    let _ = fs::create_dir_all(state_dir());
    rotate_big_logs(); // 全新会话开始前，轮转超限的会话日志
    // 记录当前日志字节偏移：token 解析只看这之后的内容（旧运行的历史
    // token 行都在之前，绝不能被误当成当前 DSH 的 token）。
    let log_off = fs::metadata(&server_log()).map(|m| m.len()).unwrap_or(0);
    TOKEN_LOG_OFFSET.store(log_off, std::sync::atomic::Ordering::Relaxed);

    if let LaunchPlan::NodeEntry(e) = plan {
        let out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_log())
            .map_err(|_| LaunchFailure::SpawnFailed)?;
        let err = out.try_clone().map_err(|_| LaunchFailure::SpawnFailed)?;
        append_server_log(&format!(
            "starting: {} {}",
            e.node.display(),
            node_args(&e.entry)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let child = Command::new(&e.node)
            .args(node_args(&e.entry))
            .current_dir(home_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|_| LaunchFailure::NoRuntime)?;
        return Ok((child, None));
    }

    let cmd_line = match plan {
        LaunchPlan::Npx(c) | LaunchPlan::User(c) => c.clone(),
        LaunchPlan::NodeEntry(_) => unreachable!(),
    };
    if matches!(plan, LaunchPlan::Npx(_)) && which("npx").is_none() {
        return Err(LaunchFailure::NoRuntime);
    }
    let cmd_path = state_dir().join("run-dsh.cmd");
    let batch = batch_script(&home_dir(), &cmd_line, &server_log());
    fs::write(&cmd_path, batch).map_err(|_| LaunchFailure::SpawnFailed)?;
    let child = Command::new("cmd")
        .arg("/C")
        .arg(&cmd_path)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()
        .map_err(|_| LaunchFailure::SpawnFailed)?;
    Ok((child, Some(cmd_path)))
}

/// Linux/macOS：与 Windows 同构（直连 node / sh 脚本回退）；
/// 直连与脚本都自成进程组（process_group(0)），便于 killpg 整树清理。
/// 不再用 PR_SET_PDEATHSIG：多窗口模式下启动窗口退出不等于整树该杀，
/// 统一由「最后一个窗口」执行 killpg + 端口兜底；启动器崩溃时 DSH 残留，
/// 由下次启动的成员登记收养。
#[cfg(unix)]
fn spawn_hidden_dsh(plan: &LaunchPlan) -> Result<(std::process::Child, Option<PathBuf>), LaunchFailure> {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::create_dir_all(state_dir());
    rotate_big_logs(); // 全新会话开始前，轮转超限的会话日志
    let log_off = fs::metadata(&server_log()).map(|m| m.len()).unwrap_or(0);
    TOKEN_LOG_OFFSET.store(log_off, std::sync::atomic::Ordering::Relaxed);

    if let LaunchPlan::NodeEntry(e) = plan {
        let out = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_log())
            .map_err(|_| LaunchFailure::SpawnFailed)?;
        let err = out.try_clone().map_err(|_| LaunchFailure::SpawnFailed)?;
        append_server_log(&format!(
            "starting: {} {}",
            e.node.display(),
            node_args(&e.entry)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        let mut command = Command::new(&e.node);
        command
            .args(node_args(&e.entry))
            .current_dir(home_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .process_group(0);
        let child = command.spawn().map_err(|_| LaunchFailure::NoRuntime)?;
        return Ok((child, None));
    }

    let cmd_line = match plan {
        LaunchPlan::Npx(c) | LaunchPlan::User(c) => c.clone(),
        LaunchPlan::NodeEntry(_) => unreachable!(),
    };
    if matches!(plan, LaunchPlan::Npx(_)) && which("npx").is_none() {
        return Err(LaunchFailure::NoRuntime);
    }
    let script_path = state_dir().join("run-dsh.sh");
    let script = shell_script(&home_dir(), &cmd_line, &server_log());
    fs::write(&script_path, script).map_err(|_| LaunchFailure::SpawnFailed)?;
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
        .map_err(|_| LaunchFailure::SpawnFailed)?;
    let mut command = Command::new("sh");
    command.arg(&script_path).process_group(0);
    let child = command
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn()
        .map_err(|_| LaunchFailure::SpawnFailed)?;
    Ok((child, Some(script_path)))
}

fn ensure_dsh() -> Startup {
    let my_pid = std::process::id();

    // —— 端口已开：加入已有会话（登记表 + 引用计数）或纯附加 ——
    if port_open(Duration::from_millis(800)) {
        let joined = with_lock(|| -> Option<Registry> {
            let mut reg = read_registry()?;
            if !pid_alive_safe(reg.server_pid) {
                // 服务根进程已死：登记表失效（端口已被别的程序占用）
                prune_dead(&mut reg);
                if reg.members.is_empty() {
                    let _ = fs::remove_file(owner_file());
                }
                return None;
            }
            prune_dead(&mut reg); // 崩溃窗口的残留登记在此剔除（孤儿收养）
            reg.members.push(my_pid);
            reg.members.sort_unstable();
            reg.members.dedup();
            write_registry(&reg);
            Some(reg)
        });
        if let Some(reg) = joined {
            log(&format!(
                "joined DSH session ({} window(s), server pid {})",
                reg.members.len(), reg.server_pid
            ));
            #[cfg(windows)]
            let kill = if reg.job_name.is_empty() {
                None
            } else {
                KillJob::open_named(&reg.job_name).map(KillHandle::Job).or_else(|| {
                    log("WARNING: could not open named kill job; falling back to taskkill chain");
                    None
                })
            };
            #[cfg(unix)]
            let kill = Some(KillHandle::Pgid(reg.server_pid));
            return Startup::Ok { owned: true, server_pid: Some(reg.server_pid), kill, pending: None };
        }
        log("DSH already running; attaching without ownership");
        return Startup::Ok { owned: false, server_pid: None, kill: None, pending: None };
    }

    // —— 端口未开：清理陈旧登记表后全新启动 ——
    with_lock(|| {
        if let Some(mut reg) = read_registry() {
            prune_dead(&mut reg);
            if reg.members.is_empty() {
                let _ = fs::remove_file(owner_file());
                log("cleared stale registry");
            }
        }
    });

    let plan = plan_launch();
    log(&format!("DSH is not running; starting: {}", plan.describe()));
    let (child, _script) = match spawn_hidden_dsh(&plan) {
        Ok(v) => v,
        Err(why) => {
            log(&format!("failed to spawn hidden DSH: {}", why.label()));
            show_start_failure(&why.advice());
            return Startup::Failed;
        }
    };
    let pid = child.id();

    // 平台化的整树清理句柄
    #[cfg(windows)]
    let (job_name, kill) = {
        let name = job_name_for(pid);
        match KillJob::create_named(&name) {
            Some(job) if job.assign(pid) => {
                log("hidden DSH tree assigned to a named kill-on-close job");
                (name, Some(KillHandle::Job(job)))
            }
            _ => match KillJob::create().and_then(|j| j.assign(pid).then_some(j)) {
                Some(job) => {
                    log("named job unavailable; using anonymous job (other windows fall back to taskkill chain)");
                    (String::new(), Some(KillHandle::Job(job)))
                }
                None => {
                    log("WARNING: could not assign hidden DSH to a job; falling back to taskkill chain");
                    (String::new(), None)
                }
            },
        }
    };
    #[cfg(unix)]
    let job_name = String::new();
    #[cfg(unix)]
    let kill = {
        log("hidden DSH tree armed with process group kill");
        Some(KillHandle::Pgid(pid))
    };

    with_lock(|| write_registry(&Registry { server_pid: pid, job_name, members: vec![my_pid] }));
    log(&format!("hidden launcher started (pid {})", pid));

    // 端口若已立刻就绪（热缓存启动），就不进 pending，直接按已就绪走；
    // 否则把等待交给事件循环，窗口可以马上显示。
    let pending = if port_open(Duration::from_millis(200)) {
        log("DSH web UI is up");
        None
    } else {
        Some(PendingBoot {
            child,
            pid,
            deadline: Instant::now() + Duration::from_secs(wait_seconds()),
        })
    };
    Startup::Ok { owned: true, server_pid: Some(pid), kill, pending }
}

// ---------- 浏览器白名单 ----------
/// 仅允许精确等于 DSH 地址或以 "DSH地址/" 开头的地址（防止 30800 这类端口前缀绕过）。
fn url_allowed_for(base: &str, uri: &str) -> bool {
    uri == base || uri.starts_with(&format!("{}/", base))
}

/// 导航白名单判定：
/// - DSH 干净地址 / about:blank 一律放行；
/// - 本地“DSH 启动中…”占位页（wry 的 with_html 实际是 `data:text/html;base64,…`
///   导航）只在“尚未进入真实页面”时放行一次，之后 data: 地址重新被拦；
/// - 其余一律拒绝（外部 http(s) 由调用方转交系统浏览器）。
fn nav_allowed(base: &str, uri: &str, placeholder_pending: &std::sync::atomic::AtomicBool) -> bool {
    if uri == "about:blank" || url_allowed_for(base, uri) {
        return true;
    }
    uri.starts_with("data:text/html")
        && placeholder_pending.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// http(s) 地址判定：非 http(s)（如 file:、about:blank）一律不外开。
fn is_http_url(uri: &str) -> bool {
    uri.starts_with("http://") || uri.starts_with("https://")
}

/// 用系统默认浏览器打开外部 http(s) 链接。
/// DSH 窗口本身保持锁定（白名单导航不变）；用户在聊天里点开的
/// 网页搜索来源、文档链接等（前端以 target=_blank 渲染）不再"点了没反应"。
fn open_external_if_http(uri: &str) {
    if !is_http_url(uri) {
        return;
    }
    log(&format!("opening external URL in system browser: {}", uri));
    #[cfg(windows)]
    {
        // SAFETY: 两个宽字符串都以 NUL 结尾；其余参数为空指针合法。
        unsafe {
            use windows_sys::Win32::UI::Shell::ShellExecuteW;
            use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
            let wide: Vec<u16> = uri.encode_utf16().chain(std::iter::once(0)).collect();
            let op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
            let _ = ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open")
            .arg(uri)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open")
            .arg(uri)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
    }
}

// ---------- 系统托盘（Windows） ----------
/// 托盘事件：托盘消息窗口的 WndProc（事件循环线程内）经 proxy 送回主事件处理。
enum AppEvent {
    TrayRestore, // 托盘左键 / 另一实例激活：恢复窗口
    TrayExit,    // 托盘菜单「关闭」：真正关窗并按成员登记清场
}

/// WebView2 权限策略：放行 DSH 插件需要的能力，其余（麦克风/摄像头/定位/
/// 传感器/MIDI/窗口管理等）一律显式拒绝。
/// 通知权限是插件桌面通知的开关：WebView2 默认拒绝，插件看到
/// `Notification.permission !== 'granted'` 时会直接不弹（"通知从来没响过"）。
#[cfg(windows)]
fn apply_webview_permissions(webview: &WebView) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY, COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ,
        COREWEBVIEW2_PERMISSION_KIND_FILE_READ_WRITE,
        COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS,
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
        COREWEBVIEW2_PERMISSION_STATE_DENY,
    };
    use webview2_com::PermissionRequestedEventHandler;
    use wry::WebViewExtWindows;

    let core = match unsafe { webview.controller().CoreWebView2() } {
        Ok(c) => c,
        Err(e) => {
            log(&format!("WARNING: could not get CoreWebView2 for permission policy: {}", e));
            return;
        }
    };
    let handler = PermissionRequestedEventHandler::create(Box::new(|_, args| {
        let Some(args) = args else { return Ok(()) };
        let mut kind = Default::default();
        unsafe { args.PermissionKind(&mut kind)? };
        let allow = kind == COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS
            || kind == COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ
            || kind == COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS
            || kind == COREWEBVIEW2_PERMISSION_KIND_FILE_READ_WRITE
            || kind == COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY;
        let state = if allow { COREWEBVIEW2_PERMISSION_STATE_ALLOW } else { COREWEBVIEW2_PERMISSION_STATE_DENY };
        unsafe { args.SetState(state)? };
        Ok(())
    }));
    let mut token = 0i64;
    match unsafe { core.add_PermissionRequested(&handler, &mut token) } {
        Ok(()) => log("webview permission policy installed (notifications/clipboard/downloads/file-rw/autoplay allowed; mic/camera/geo denied)"),
        Err(e) => log(&format!("WARNING: could not install permission policy: {}", e)),
    }
}

/// 新窗口（window.open / target=_blank）策略：
/// - DSH 同源地址 → `Allow`：由 WebView2 在**同一环境/同一用户数据目录**下开窗，
///   cookie 与会话共享，不会再出现"甩给系统浏览器 → 401"；
/// - 其它 http(s) → 系统默认浏览器，然后拒绝；
/// - 其余协议 → 拒绝。
///
/// 备注：wry 还提供 `NewWindowResponse::Create { webview }`（把自建 WebView 交给
/// 它当新窗口）。实测在该 WebView2 版本上会让 opener 的 `window.open()` 永不返回
/// （页面 JS 卡死），因此不用它。
#[cfg(windows)]
fn popup_new_window_response(uri: &str, base_url: &str) -> wry::NewWindowResponse {
    if url_allowed_for(base_url, uri) {
        log(&format!("opening same-origin URL in a WebView2 popup window: {}", uri));
        return wry::NewWindowResponse::Allow;
    }
    open_external_if_http(uri);
    wry::NewWindowResponse::Deny
}

#[cfg(windows)]
const WM_TRAYMSG: u32 = 0x8000 + 1; // WM_APP + 1（本程序私有的托盘回调消息）

#[cfg(windows)]
const TRAY_CMD_EXIT: usize = 2; // 「关闭」
#[cfg(windows)]
const TRAY_CMD_CLOSE_EXIT: usize = 30; // 设置：直接关闭DSH Launch Console
#[cfg(windows)]
const TRAY_CMD_CLOSE_TRAY: usize = 31; // 设置：最小化DSH Launch Console

/// 32x32 RGBA -> 32bpp BGRA -> HICON（托盘图标；32 像素源在高 DPI 下
/// 缩放到 16/24/32 的托盘槽位时仍保持清晰）。
/// 注意：CreateIcon 的 XOR 位图是 DDB（设备相关位图，行序自上而下），
/// 不能按 DIB 规则翻转行序，否则图标会上下镜像。
#[cfg(windows)]
fn tray_hicon() -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::CreateIcon;
        let s = TRAY_ICON_SIZE as usize;
        let mut bgra = vec![0u8; s * s * 4];
        for (i, px) in TRAY_ICON_RGBA.chunks_exact(4).enumerate() {
            let dst = i * 4;
            bgra[dst] = px[2];     // B
            bgra[dst + 1] = px[1]; // G
            bgra[dst + 2] = px[0]; // R
            bgra[dst + 3] = px[3]; // A
        }
        let mask = vec![0u8; s * (s / 8)]; // AND 掩码每行 1 位/像素，全 0：透明由 alpha 决定
        CreateIcon(
            std::ptr::null_mut(),
            TRAY_ICON_SIZE as i32,
            TRAY_ICON_SIZE as i32,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
    }
}

/// 托盘图标状态；Drop 时摘除托盘图标并销毁 HICON。
#[cfg(windows)]
struct TrayData {
    nid: windows_sys::Win32::UI::Shell::NOTIFYICONDATAW,
    icon: windows_sys::Win32::UI::WindowsAndMessaging::HICON,
}

#[cfg(windows)]
impl TrayData {
    fn add(hwnd: isize) -> Option<Self> {
        unsafe {
            use windows_sys::Win32::UI::Shell::{
                Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
            };
            use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
            let icon = tray_hicon();
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd as windows_sys::Win32::Foundation::HWND;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAYMSG;
            nid.hIcon = icon;
            let tip: Vec<u16> = "DSH Launch Console"
                .encode_utf16().chain(std::iter::once(0)).collect();
            for (d, s) in nid.szTip.iter_mut().zip(tip) {
                *d = s;
            }
            if Shell_NotifyIconW(NIM_ADD, &nid) != 0 {
                Some(Self { nid, icon })
            } else {
                DestroyIcon(icon);
                None
            }
        }
    }
}

#[cfg(windows)]
impl Drop for TrayData {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE};
            use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.nid);
            let _ = DestroyIcon(self.icon);
        }
    }
}

// ---------- ✕ 按钮行为设置（托盘菜单「设置」可切换，持久化） ----------
/// 状态文件内容：tray = 点 ✕ 最小化到托盘（默认）；exit = 点 ✕ 直接关闭。
#[cfg(windows)]
fn close_action_file() -> PathBuf {
    state_dir().join("close-action")
}

/// 当前 ✕ 行为是否为“最小化到托盘”。默认是（无状态文件时）。
#[cfg(windows)]
fn close_to_tray() -> bool {
    fs::read_to_string(close_action_file())
        .map(|s| s.trim() == "tray")
        .unwrap_or(true)
}

#[cfg(windows)]
fn set_close_to_tray(v: bool) {
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(close_action_file(), if v { "tray" } else { "exit" });
    log(&format!("close-button action set to {}", if v { "tray" } else { "exit" }));
}

/// 右键托盘图标：弹出菜单（TPM_RETURNCMD 同步返回所选命令）。
/// 菜单结构（按用户指定顺序与文案）：
///   设置 ▸ 直接关闭DSH Launch Console / 最小化DSH Launch Console（勾选 = 当前生效的 ✕ 行为）
///   关闭
#[cfg(windows)]
fn tray_popup(hwnd: isize) -> usize {
    unsafe {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW,
            SetForegroundWindow, TrackPopupMenu, MF_CHECKED, MF_POPUP, MF_STRING,
            TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        };
        let menu = CreatePopupMenu();
        let settings = CreatePopupMenu();
        let exit: Vec<u16> = "关闭"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let close_exit: Vec<u16> = "直接关闭DSH Launch Console"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let close_tray: Vec<u16> = "最小化DSH Launch Console"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let settings_label: Vec<u16> = "设置"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let _ = AppendMenuW(menu, MF_POPUP, settings as usize, settings_label.as_ptr());
        let _ = AppendMenuW(menu, MF_STRING, TRAY_CMD_EXIT, exit.as_ptr());
        // 设置子菜单：两项互斥选择，勾选 = 当前生效的 ✕ 行为
        let tray_mode = close_to_tray();
        let _ = AppendMenuW(
            settings,
            MF_STRING | if tray_mode { 0 } else { MF_CHECKED },
            TRAY_CMD_CLOSE_EXIT,
            close_exit.as_ptr(),
        );
        let _ = AppendMenuW(
            settings,
            MF_STRING | if tray_mode { MF_CHECKED } else { 0 },
            TRAY_CMD_CLOSE_TRAY,
            close_tray.as_ptr(),
        );

        let mut pt: POINT = std::mem::zeroed();
        let _ = GetCursorPos(&mut pt);
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        // 置前必须用主窗口（消息窗口不可见、无法成为前台）；窗口隐藏时此调用
        // 会失败，菜单仍可正常点选，仅"点菜单外自动关闭"在小概率下需要按 Esc。
        let main = MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed)
            as windows_sys::Win32::Foundation::HWND;
        if !main.is_null() {
            let _ = SetForegroundWindow(main);
            let _ = PostMessageW(main, 0, 0, 0); // WM_NULL：让菜单可被外部点击关闭
        }
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x, pt.y, 0,
            hwnd,
            std::ptr::null(),
        );
        let _ = DestroyMenu(menu); // 子菜单随父菜单一起销毁
        match cmd as usize {
            TRAY_CMD_CLOSE_EXIT => {
                set_close_to_tray(false);
                0
            }
            TRAY_CMD_CLOSE_TRAY => {
                set_close_to_tray(true);
                0
            }
            other => other,
        }
    }
}

/// 真正退出：按成员登记退出（最后一个窗口才杀 DSH），然后结束事件循环。
fn request_exit(
    control_flow: &mut ControlFlow,
    owned: bool,
    server_pid: Option<u32>,
    kill: &mut Option<KillHandle>,
) {
    if owned {
        if let Some(pid) = server_pid {
            leave_server(std::process::id(), pid, kill.take());
        }
    }
    log("launcher finished");
    #[cfg(windows)]
    let _ = fs::remove_file(state_dir().join("tray-hwnd"));
    *control_flow = ControlFlow::Exit;
}

// ---------- 单实例（同时只允许一个窗口 / 一个托盘图标） ----------
/// 单实例互斥体名（`DSH_LAUNCH_CONSOLE_MUTEX` 可覆盖：让并存的测试实例/多套配置互不干扰）。
fn single_instance_name() -> String {
    env_or("DSH_LAUNCH_CONSOLE_MUTEX", "DSH_Launch_Console_SingleInstance_Mutex")
}

/// Windows：尝试成为唯一实例（命名互斥体，进程存活期间持有）。
/// 失败时通知已有实例恢复窗口并返回 false（调用方应直接退出）。
#[cfg(windows)]
fn try_become_single_instance() -> bool {
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, PostMessageW, RegisterWindowMessageW,
        };
        let name: Vec<u16> = single_instance_name()
            .encode_utf16().chain(std::iter::once(0)).collect();
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if handle.is_null() {
            // 互斥体创建失败：按第一实例继续（登记表机制兜底）
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            // 通知已有实例：读状态目录里的托盘消息窗口句柄，投递“激活”消息，
            // 恢复/聚焦它的主窗口（不依赖 FindWindow 系列 API）
            let msg_name: Vec<u16> = "DSH_Launch_Console_Activate"
                .encode_utf16().chain(std::iter::once(0)).collect();
            let msg = RegisterWindowMessageW(msg_name.as_ptr());
            let hwnd = fs::read_to_string(state_dir().join("tray-hwnd"))
                .ok()
                .and_then(|s| s.trim().parse::<isize>().ok())
                .unwrap_or(0);
            if hwnd != 0 && msg != 0 {
                let _ = AllowSetForegroundWindow(u32::MAX); // ASFW_ANY：允许第一实例夺回前台
                let _ = PostMessageW(
                    hwnd as windows_sys::Win32::Foundation::HWND,
                    msg,
                    0,
                    0,
                );
            }
            return false;
        }
        // 第一实例：互斥体句柄保持到进程结束（崩溃/退出由系统自动释放）。
        // 裸 HANDLE 无 Drop，只要不显式 CloseHandle 就是持续持有。
        true
    }
}

/// Windows：另一实例发来的“激活窗口”消息 id（按会话注册，各进程一致）。
#[cfg(windows)]
fn activate_message() -> u32 {
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
        let name: Vec<u16> = "DSH_Launch_Console_Activate"
            .encode_utf16().chain(std::iter::once(0)).collect();
        RegisterWindowMessageW(name.as_ptr())
    }
}

// ---------- 托盘消息窗口（经典 Win32 方案，不依赖 tao 的消息钩子） ----------
// 托盘回调消息由系统直接投递给我们自己的 message-only 窗口，
// 其 WndProc 运行在事件循环线程内（与 tao 同一消息泵），
// 收到的动作经 EventLoopProxy 送回主事件处理——路径确定、无钩子时序问题。
#[cfg(windows)]
static TRAY_PROXY: std::sync::OnceLock<EventLoopProxy<AppEvent>> = std::sync::OnceLock::new();
#[cfg(windows)]
static TRAY_ACTIVATE_MSG: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(windows)]
static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

#[cfg(windows)]
const TRAY_WND_CLASS: &str = "DSH_Launch_Console_TrayWnd";

#[cfg(windows)]
unsafe extern "system" fn tray_wndproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP,
    };
    let send = |e: AppEvent| {
        if let Some(p) = TRAY_PROXY.get() {
            let _ = p.send_event(e);
        }
    };
    if msg == WM_TRAYMSG {
        // 托盘回调：wParam=图标 id，lParam=鼠标事件
        match lparam as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => send(AppEvent::TrayRestore),
            WM_RBUTTONUP => match tray_popup(hwnd as isize) {
                TRAY_CMD_EXIT => send(AppEvent::TrayExit),
                _ => {}
            },
            _ => {}
        }
        return 0;
    }
    let activate = TRAY_ACTIVATE_MSG.load(std::sync::atomic::Ordering::Relaxed);
    if activate != 0 && msg == activate {
        // 另一实例启动：恢复/聚焦本窗口（含隐藏到托盘的情况）
        send(AppEvent::TrayRestore);
        return 0;
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// 用 exe 内嵌的多帧 .ico 资源（id=1，winres `set_icon` 的默认 id）设置
/// 窗口大小图标：标准组图标是任务栏渲染最稳的格式——tao 从 RGBA 经
/// `CreateIcon` 合成的 256×256 图标其 AND 掩码按“每像素 1 字节”构造，
/// 不符合“每像素 1 位打包”的格式要求，部分系统的任务栏因此不显示。
#[cfg(windows)]
fn apply_resource_window_icons(hwnd: isize) {
    unsafe {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            LoadImageW, SendMessageW, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTSIZE,
            LR_SHARED, WM_SETICON,
        };
        let hinst = GetModuleHandleW(std::ptr::null());
        let big = LoadImageW(hinst, 1usize as *const u16, IMAGE_ICON, 32, 32, LR_DEFAULTSIZE | LR_SHARED);
        let small = LoadImageW(hinst, 1usize as *const u16, IMAGE_ICON, 16, 16, LR_DEFAULTSIZE | LR_SHARED);
        if big.is_null() || small.is_null() {
            log("WARNING: could not load window icons from exe resource");
            return;
        }
        let _ = SendMessageW(hwnd as HWND, WM_SETICON, ICON_BIG as usize, big as isize);
        let _ = SendMessageW(hwnd as HWND, WM_SETICON, ICON_SMALL as usize, small as isize);
        log("window icons set from exe resource (multi-frame .ico)");
    }
}

/// 创建接收托盘回调的 message-only 窗口（当前线程 = 事件循环线程）。
#[cfg(windows)]
fn create_tray_window() -> windows_sys::Win32::Foundation::HWND {
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, RegisterClassW, WNDCLASSW, HWND_MESSAGE,
        };
        let class: Vec<u16> = TRAY_WND_CLASS
            .encode_utf16().chain(std::iter::once(0)).collect();
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(tray_wndproc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.lpszClassName = class.as_ptr();
        let _ = RegisterClassW(&wc); // 重复注册返回 0，无害
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0, 0, 0, 0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    }
}

/// Unix：SIGUSR1 信号置位，事件循环轮询后恢复窗口。
#[cfg(unix)]
static ACTIVATE_FLAG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_sigusr1(_: libc::c_int) {
    ACTIVATE_FLAG.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Unix：对 state_dir/single.lock 加 flock 并写入本进程 pid。
/// 返回 Some(锁文件) = 本进程是第一实例；None = 已通知现有实例，应退出。
#[cfg(unix)]
fn acquire_single_instance() -> Option<std::fs::File> {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::unix::io::AsRawFd;
    let _ = fs::create_dir_all(state_dir());
    let path = state_dir().join(format!("{}.lock", single_instance_name()));
    let file = fs::OpenOptions::new().create(true).write(true).open(&path).ok()?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        // 第一实例：写入 pid 供后续实例发 SIGUSR1；安装信号处理
        let mut f = &file;
        let _ = f.set_len(0);
        let _ = f.seek(SeekFrom::Start(0));
        let _ = write!(f, "{}", std::process::id());
        unsafe { libc::signal(libc::SIGUSR1, on_sigusr1 as usize); }
        Some(file)
    } else {
        // 已有实例：向它发 SIGUSR1（恢复窗口）
        if let Ok(text) = fs::read_to_string(&path) {
            if let Ok(pid) = text.trim().parse::<i32>() {
                unsafe { libc::kill(pid, libc::SIGUSR1); }
            }
        }
        None
    }
}

// ---------- 主流程 ----------
fn main() {
    // 显式 AppUserModelID：未打包应用缺省 AUMID 时，部分 Windows 版本的
    // 任务栏按钮图标可能显示为空白/默认图标。
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let id: Vec<u16> = "DSH.LaunchConsole"
            .encode_utf16().chain(std::iter::once(0)).collect();
        let _ = SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
    }
    log("launcher started");
    // 清理历史遗留的端口扫描临时文件（正常流程用后即删；被强杀时可能残留一个）
    let _ = fs::remove_file(std::env::temp_dir().join("dsh-netstat.tmp"));
    // 更新时若旧 exe 仍在运行，会先把旧文件改名成 <exe>.old 再写入新文件
    // （Windows 不允许删除正在运行的 exe）：这里顺手清掉上一轮留下的 .old。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(name) = exe.file_name().map(|s| s.to_string_lossy().into_owned()) {
            let stale = exe.with_file_name(format!("{}.old", name));
            if stale.is_file() {
                let _ = fs::remove_file(&stale);
            }
        }
    }

    // 诊断开关：只解析并打印"会怎么启动 dsh"，不启动任何进程（也不占单实例闸门）。
    if std::env::args().any(|a| a == "--print-launch-plan" || a == "--diagnose-launch") {
        let plan = plan_launch();
        let detail = match resolve_dsh_entry() {
            Ok(e) => format!("node={} entry={} source={}", e.node.display(), e.entry.display(), e.source),
            Err(why) => format!("resolve failed: {}", why.label()),
        };
        log(&format!("launch plan: {}", plan.describe()));
        log(&format!("launch plan detail: {}", detail));
        log(&format!("target url: {} (host/port from DSH_LAUNCH_CONSOLE_URL)", dsh_url()));
        log(&format!("node on PATH: {:?}", which("node")));
        log(&format!("dsh on PATH: {:?}", which("dsh")));
        #[cfg(windows)]
        unsafe {
            // GUI 子系统没有 stdout：附加到父控制台，方便在终端里直接看结果
            use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
            if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
                println!("launch plan: {}", plan.describe());
                println!("launch plan detail: {}", detail);
            }
        }
        return;
    }

    // 单实例闸门：已有实例在运行 → 通知它恢复窗口，本进程直接退出
    #[cfg(windows)]
    {
        if !try_become_single_instance() {
            log("another instance is running; activating its window and exiting");
            return;
        }
    }
    #[cfg(unix)]
    let _single_lock = acquire_single_instance();
    #[cfg(unix)]
    {
        if _single_lock.is_none() {
            log("another instance is running; activating its window and exiting");
            return;
        }
    }

    let (owned, server_pid, kill, pending) = match ensure_dsh() {
        Startup::Failed => return,
        Startup::Ok { owned, server_pid, kill, pending } => (owned, server_pid, kill, pending),
    };
    log(&format!("opening browser window (owned={})", owned));
    run_browser(owned, server_pid, kill, pending);
}

// ---------- 窗口图标（DSH 黑色鲸鱼，RGBA 编译期嵌入，多尺寸同源） ----------
// 标题栏：16x16（ICON_SMALL 原生尺寸，免缩放；alpha 已二值化避免边缘噪点）。
// 任务栏：256x256（经 tao 的 with_taskbar_icon 单独设置 ICON_BIG，任意 DPI 都清晰）。
// 托盘：  32x32（见 tray_hicon；缩放进 16/24/32 的托盘槽位保持清晰）。
const WINDOW_ICON_RGBA: &[u8] = include_bytes!("icon-16.rgba");
const WINDOW_ICON_SIZE: u32 = 16;
#[cfg(windows)]
const TASKBAR_ICON_RGBA: &[u8] = include_bytes!("icon-256.rgba");
#[cfg(windows)]
const TASKBAR_ICON_SIZE: u32 = 256;
#[cfg(windows)]
const TRAY_ICON_RGBA: &[u8] = include_bytes!("icon-32.rgba");
#[cfg(windows)]
const TRAY_ICON_SIZE: u32 = 32;

fn run_browser(
    owned: bool,
    server_pid: Option<u32>,
    kill: Option<KillHandle>,
    pending: Option<PendingBoot>,
) {
    let mut kill = kill;
    let mut pending = pending;

    // 事件循环 + 托盘消息窗口：托盘回调由专用 message-only 窗口接收
    // （WndProc 运行在事件循环线程内），经 proxy 送回主事件处理——
    // 不依赖 tao 的消息钩子，路径确定。
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    #[cfg(windows)]
    {
        let _ = TRAY_PROXY.set(event_loop.create_proxy());
        TRAY_ACTIVATE_MSG.store(activate_message(), std::sync::atomic::Ordering::Relaxed);
    }

    let window_icon = tao::window::Icon::from_rgba(
        WINDOW_ICON_RGBA.to_vec(),
        WINDOW_ICON_SIZE,
        WINDOW_ICON_SIZE,
    )
    .ok();
    // 任务栏大图标（ICON_BIG）：256x256 高清鲸鱼，任意 DPI 缩放都清晰
    #[cfg(windows)]
    let taskbar_icon = tao::window::Icon::from_rgba(
        TASKBAR_ICON_RGBA.to_vec(),
        TASKBAR_ICON_SIZE,
        TASKBAR_ICON_SIZE,
    )
    .ok();
    let mut builder = WindowBuilder::new()
        .with_title("DSH Launch Console")
        .with_window_icon(window_icon)
        .with_inner_size(LogicalSize::new(1320.0, 900.0))
        .with_min_inner_size(LogicalSize::new(800.0, 600.0))
        // 先隐藏创建：任务栏按钮在窗口首次可见时创建并缓存图标，若图标在
        // 显示之后才 WM_SETICON，按钮不会自动刷新（Windows 已知怪癖）——
        // 所以先隐藏、设好图标、再显示。
        .with_visible(false);
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowBuilderExtWindows;
        builder = builder.with_taskbar_icon(taskbar_icon);
    }
    let window = match builder.build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => {
            show_error("DSH Launch Console", &format!("创建窗口失败: {}", e));
            if owned { if let Some(pid) = server_pid { leave_server(std::process::id(), pid, kill.take()); } }
            return;
        }
    };

    // 系统托盘：点 ✕ 的行为由设置决定（默认隐藏到托盘；可改为直接关闭）。
    // 托盘图标挂在专用消息窗口上；托盘不可用（极少数环境）时退回关窗即退出。
    #[cfg(windows)]
    let tray_hwnd: isize = {
        use tao::platform::windows::WindowExtWindows;
        let main_hwnd = window.hwnd();
        MAIN_HWND.store(main_hwnd as isize, std::sync::atomic::Ordering::Relaxed);
        // 任务栏图标修复：改用 exe 内嵌多帧 .ico 资源直接 WM_SETICON，
        // 覆盖 tao 从 RGBA 合成的图标（其掩码格式任务栏可能不认）
        apply_resource_window_icons(main_hwnd);
        let w = create_tray_window() as isize;
        // 单实例通知用：把托盘消息窗口句柄写入状态目录，后续实例直接读取——
        // 不依赖 FindWindow 系列枚举（message-only 窗口的跨进程枚举不可靠）
        let _ = fs::create_dir_all(state_dir());
        let _ = fs::write(state_dir().join("tray-hwnd"), w.to_string());
        log(&format!("tray message window hwnd={}", w));
        w
    };
    #[cfg(windows)]
    let mut tray = {
        match TrayData::add(tray_hwnd) {
            Some(t) => Some(t),
            None => {
                log("WARNING: tray icon unavailable; close button will exit");
                None
            }
        }
    };

    let base_url = dsh_url();
    // DSH 还在冷启动（pending）：此刻必然没有可用 token，直接显示「启动中」页，
    // 端口就绪等待交给事件循环——窗口立刻可见，不再白等 1~2 秒。
    let booting = pending.is_some();
    // 已就绪的 token 来源：本程序启动的 DSH 日志行(可信) → 终端抄录/剪贴板(HTTP 校验)。
    // owned 且已就绪时只读日志（最快，不走 netstat 抄录/剪贴板的几百毫秒开销）；
    // 万一日志里还没有，下面的后台轮询第一拍就会补上抄录/剪贴板通道。
    let startup_token = if booting {
        None
    } else if owned {
        latest_web_token()
    } else {
        ready_token(false)
    };
    // 初始页面决策：带 token 秒开 / 无鉴权服务直接秒开 / 本地“启动中”页 / 附加纯地址。
    // 注意：预检是“无 cookie 的裸 HTTP 请求”，对有鉴权的 DSH 必然得到 401，
    // 无法反映 WebView 内已持久化的 cookie 是否有效——所以只在 owned 模式
    // 用它识别“无鉴权的旧版 DSH”；附加模式一律静默加载纯地址：
    // 有 cookie 直接进界面、没有则显示 DSH 自己的 401 提示页，不弹任何窗。
    // 冷启动阶段省掉这次预检（端口还没开，白等一次连接超时）。
    let cookie_status = if owned && !booting { dsh_http_status() } else { None };
    let initial = if booting {
        InitialPage::Loading
    } else {
        choose_initial_page(owned, startup_token.as_deref(), cookie_status.map(|s| s == 200))
    };
    if matches!(&initial, InitialPage::Url(u) if u.contains("?token=")) {
        log("startup token ready; opening authenticated URL directly");
    }
    // 需要后台补 token：owned 冷启动(加载页) / 附加模式没秒开 token(盯终端与剪贴板)
    let need_token_heal = if owned {
        matches!(initial, InitialPage::Loading)
    } else {
        startup_token.is_none()
    };
    if !owned {
        log("attach mode: loading plain URL (auth via persisted cookie)");
    }
    // WebView2 用户数据目录固定到状态目录（cookie 不跟 exe 名走）
    let mut web_context = wry::WebContext::new(Some(webview_data_dir()));
    let mut builder = WebViewBuilder::new_with_web_context(&mut web_context);
    builder = match &initial {
        InitialPage::Url(u) => builder.with_url(u),
        InitialPage::Loading => builder.with_html(loading_html()),
    };
    // wry 的 with_html 实际是以 `data:text/html;charset=utf-8;base64,…` 导航的，
    // 会被下面的白名单拦掉（窗口一片空白/黑）。占位页只在“还没进真实页面”时
    // 存在，所以给首个 data: 导航开一次口，真实 DSH 页面一加载即恢复严格白名单。
    let placeholder_nav_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(matches!(
        initial,
        InitialPage::Loading
    )));
    let placeholder_flag = placeholder_nav_pending.clone();
    // base_url 会被下面的导航闭包 move 走，新窗口回调单独留一份
    let base_url_for_popup = base_url.clone();
    let mut builder = builder.with_devtools(false);
    // 自动播放：新版 WebView2 会在权限事件里问（见 apply_webview_permissions），
    // 老版本不发权限事件，只能用浏览器参数兜底放行。
    #[cfg(windows)]
    {
        use wry::WebViewBuilderExtWindows;
        builder = builder.with_additional_browser_args("--autoplay-policy=no-user-gesture-required");
    }
    let webview = builder
        .with_navigation_handler(move |uri| {
            // about:blank：空页放行；占位页的 data: URL 放行一次；
            // 主窗口导航：仅 DSH 干净地址在白名单内（带 token 的地址重定向
            // 回 `/` 时不能被拦）；外部 http(s) 转交系统默认浏览器。
            if nav_allowed(&base_url, &uri, &placeholder_flag) {
                return true;
            }
            open_external_if_http(&uri);
            false
        })
        .with_new_window_req_handler(move |uri, _features| {
            #[cfg(windows)]
            {
                popup_new_window_response(&uri, &base_url_for_popup)
            }
            #[cfg(not(windows))]
            {
                // Linux/macOS：同源弹窗暂不支持壳内承载，外链交给系统浏览器
                if !url_allowed_for(&base_url_for_popup, &uri) {
                    open_external_if_http(&uri);
                }
                wry::NewWindowResponse::Deny
            }
        })
        .build(&window);
    let webview = match webview {
        Ok(w) => w,
        Err(e) => {
            show_error(
                "DSH Launch Console",
                &format!("WebView 初始化失败: {}\n\nLinux 需要 webkit2gtk（libwebkit2gtk-4.1-0），Windows 需要 Edge WebView2 运行时。", e),
            );
            if owned { if let Some(pid) = server_pid { leave_server(std::process::id(), pid, kill.take()); } }
            return;
        }
    };
    // 权限策略：在显示窗口前装好（通知/剪贴板/下载等插件能力由此可用）
    #[cfg(windows)]
    apply_webview_permissions(&webview);

    // 图标已在窗口隐藏时设置完毕，现在才显示窗口——任务栏按钮创建时
    // 就能拿到正确图标；显示后再补一次 WM_SETICON 双保险。
    window.set_visible(true);
    #[cfg(windows)]
    {
        let _ = window.set_focus();
        apply_resource_window_icons(MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed) as isize);
    }

    log("browser window shown");
    // tao 的 run() 永不返回：进程清理必须在事件回调里完成。
    // kill 句柄移入闭包：即使走不到回调，其 Drop（Windows）也会触发内核清理。
    let mut exiting = false;
    // 首次获得焦点时再补一次窗口图标（任务栏按钮刷新兜底，只做一次）
    #[cfg(windows)]
    let mut icons_refreshed = false;
    // 新版 DSH 的 token 打印可能晚于端口就绪数秒：后台轮询，拿到后立即
    // load_url 带 token 的地址（服务端换发 cookie 并 303 回 `/`）。
    // 轮询按时间计（事件回调可能比 500ms 更频繁），窗口 90 秒。
    let mut token_applied = !need_token_heal;
    let mut applied_token: Option<String> = startup_token;
    let mut token_gave_up = false;
    let mut tick_count: u64 = 0;
    let mut token_deadline = Instant::now() + Duration::from_secs(90);
    event_loop.run(move |event, _, control_flow| {
        if exiting {
            *control_flow = ControlFlow::Exit;
            return;
        }
        #[cfg(windows)]
        {
            if !token_applied && Instant::now() < token_deadline {
                // 轮询 token 期间定期醒来（约 300ms 一次，最多 90 秒）
                *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(300));
            } else {
                *control_flow = ControlFlow::Wait;
            }
        }
        #[cfg(unix)]
        {
            // 定期醒来：轮询 SIGUSR1 激活标志（另一实例请求恢复窗口）+ token
            *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));
            if ACTIVATE_FLAG.swap(false, std::sync::atomic::Ordering::Relaxed) {
                if window.is_minimized() {
                    window.set_minimized(false);
                }
                window.set_visible(true);
                window.set_focus();
            }
        }
        // —— DSH 冷启动阶段：窗口已显示「启动中」页，这里轮询端口就绪 ——
        // 就绪 → 立刻转入 token 轮询；进程提前退出/超时 → 报错退出（整树清理）。
        if pending.is_some() {
            let pid = pending.as_ref().map(|p| p.pid).unwrap_or(0);
            match boot_poll(&mut pending) {
                BootPoll::Ready => {
                    log("DSH web UI is up");
                    token_deadline = Instant::now() + Duration::from_secs(90);
                    *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(200));
                }
                BootPoll::Continue => {
                    *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(200));
                }
                BootPoll::Exited(code) => {
                    // 失败分类：从服务日志尾部识别已知签名（端口占用/npm 网络/
                    // 权限/依赖缺失），并把"第一条错误线索"带进弹窗。
                    let tail = log_tail(&server_log(), 40);
                    let kind = classify_log_tail(&tail).unwrap_or(LaunchFailure::Exited(code));
                    log(&format!("DSH exited quickly with code {} [{}]", code, kind.label()));
                    append_server_log(&format!("exited with code {}", code));
                    let mut why = kind.advice();
                    if let Some(clue) = first_error_clue(&tail) {
                        why.push_str(&format!("\n\n首条错误线索:\n{}", clue));
                    }
                    abandon_boot(pid, None, false);
                    show_start_failure(&why);
                    exiting = true;
                    #[cfg(windows)]
                    {
                        drop(tray.take());
                    }
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                BootPoll::Timeout => {
                    let tail = log_tail(&server_log(), 40);
                    let kind = classify_log_tail(&tail).unwrap_or(LaunchFailure::Timeout);
                    log(&format!("DSH did not come up in time [{}]", kind.label()));
                    append_server_log("timeout waiting for the web UI");
                    let mut why = kind.advice();
                    if let Some(clue) = first_error_clue(&tail) {
                        why.push_str(&format!("\n\n首条错误线索:\n{}", clue));
                    }
                    abandon_boot(pid, kill.take(), true);
                    show_start_failure(&why);
                    exiting = true;
                    #[cfg(windows)]
                    {
                        drop(tray.take());
                    }
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
        }
        // token 后台轮询（两端通用）：日志(owned) → 终端抄录 → 剪贴板
        // （DSH 未就绪时不做，等上面那段把端口等出来）
        if !token_applied && pending.is_none() {
            if Instant::now() < token_deadline {
                tick_count += 1;
                // 日志/剪贴板每拍查一次；终端抄录较重(netstat+AttachConsole)，
                // 每 10 拍(约 5 秒)做一次。附加模式下日志 token 不可信，只看
                // 终端/剪贴板（均经 HTTP 校验）。
                let found = if tick_count % 10 == 1 {
                    ready_token(owned)
                } else if owned {
                    latest_web_token().or_else(clipboard_token_valid)
                } else {
                    clipboard_token_valid()
                };
                if let Some(t) = found {
                    if applied_token.as_deref() != Some(t.as_str()) {
                        applied_token = Some(t.clone());
                        let turl = format!("{}?token={}", dsh_url().trim_end_matches('/'), t);
                        let _ = webview.load_url(&turl);
                        token_applied = true;
                        log("applied web token; reloading authenticated URL");
                    }
                }
            } else if !token_gave_up {
                token_gave_up = true;
                if owned {
                    log("web token never appeared; trying plain URL");
                    let _ = webview.load_url(&dsh_url());
                } else {
                    log("no token found in logs, terminal or clipboard; staying on current page");
                }
            }
        }
        match event {
            // 首次获得焦点：补发一次窗口图标（任务栏按钮刷新兜底）
            Event::WindowEvent { event: WindowEvent::Focused(true), .. } => {
                #[cfg(windows)]
                if !icons_refreshed {
                    icons_refreshed = true;
                    apply_resource_window_icons(MAIN_HWND.load(std::sync::atomic::Ordering::Relaxed) as isize);
                }
            }
            // 托盘左键：恢复窗口
            Event::UserEvent(AppEvent::TrayRestore) => {
                if window.is_minimized() {
                    window.set_minimized(false);
                }
                window.set_visible(true);
                window.set_focus();
            }
            // 托盘菜单「关闭」：走与关窗相同的退出路径
            Event::UserEvent(AppEvent::TrayExit) => {
                exiting = true;
                #[cfg(windows)]
                {
                    drop(tray.take()); // 先摘掉托盘图标
                }
                request_exit(control_flow, owned, server_pid, &mut kill);
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // Windows：点 ✕ 的行为由设置决定——最小化到托盘（默认）或直接关闭。
                // Linux/macOS 无托盘，关窗即退出。
                #[cfg(windows)]
                {
                    if tray.is_some() && close_to_tray() {
                        window.set_visible(false);
                        log("window hidden to system tray");
                    } else {
                        exiting = true;
                        request_exit(control_flow, owned, server_pid, &mut kill);
                    }
                }
                #[cfg(not(windows))]
                {
                    exiting = true;
                    request_exit(control_flow, owned, server_pid, &mut kill);
                }
            }
            Event::WindowEvent { event: WindowEvent::Destroyed, .. } => {
                // 窗口被真正销毁（异常路径/系统强制关闭）：按退出处理
                exiting = true;
                #[cfg(windows)]
                {
                    drop(tray.take());
                }
                request_exit(control_flow, owned, server_pid, &mut kill);
            }
            _ => {}
        }
    });
}

// ---------- 测试 ----------
#[cfg(test)]
mod tests {
    use super::*;

    fn write_cmd(path: &Path, content: &str) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn kill_probe() {
        // 作业对象机制探针：直接子进程 node 服务器 + TerminateJobObject
        let child = Command::new("node")
            .args(["-e", "require('http').createServer((q,s)=>s.end('x')).listen(3199)"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn();
        let node_pid = match child {
            Ok(c) => c.id(),
            Err(e) => { println!("probe: spawn node FAILED: {}", e); return; }
        };
        let mut up = false;
        for _ in 0..20 {
            if port_open(Duration::from_millis(400)) { up = true; break; }
            thread::sleep(Duration::from_millis(400));
        }
        println!("probe: node pid = {}, server up = {}", node_pid, up);
        if !up { return; }

        match KillJob::create() {
            None => println!("probe: job create FAILED"),
            Some(job) => {
                println!("probe: assign = {}", job.assign(node_pid));
                job.kill_all();
                thread::sleep(Duration::from_millis(600));
                println!("probe: port after TerminateJobObject = {}", port_open(Duration::from_millis(400)));
                drop(job); // KILL_ON_JOB_CLOSE：内核级清理
                thread::sleep(Duration::from_millis(1500));
                println!("probe: port after job handle close = {}", port_open(Duration::from_millis(400)));
            }
        }
    }

    #[test]
    fn url_guard() {
        let base = "http://127.0.0.1:3080";
        assert!(url_allowed_for(base, "http://127.0.0.1:3080"));
        assert!(url_allowed_for(base, "http://127.0.0.1:3080/"));
        assert!(url_allowed_for(base, "http://127.0.0.1:3080/some/path?q=1#x"));
        assert!(!url_allowed_for(base, "https://127.0.0.1:3080/"));
        assert!(!url_allowed_for(base, "http://127.0.0.1:30800/"));
        assert!(!url_allowed_for(base, "https://example.com"));
        assert!(!url_allowed_for(base, "file:///C:/windows/system32/x.html"));
        assert!(!url_allowed_for(base, "http://evil.com/?next=http://127.0.0.1:3080/"));
    }

    /// 占位页放行规则：wry 的 with_html 用 data: URL 导航，必须放行一次，
    /// 否则窗口只有黑底（真实 DSH 页面一加载就恢复严格白名单）。
    #[test]
    fn placeholder_nav_guard() {
        use std::sync::atomic::AtomicBool;
        let base = "http://127.0.0.1:3080";

        // 初始就进真实页面：data: 一律拒绝
        let no_placeholder = AtomicBool::new(false);
        assert!(nav_allowed(base, base, &no_placeholder));
        assert!(nav_allowed(base, "about:blank", &no_placeholder));
        assert!(!nav_allowed(base, "data:text/html;charset=utf-8;base64,PCFkb2N0eXBl", &no_placeholder));

        // 占位页：首次 data: 放行，之后拒绝
        let pending = AtomicBool::new(true);
        assert!(nav_allowed(base, "data:text/html;charset=utf-8;base64,PCFkb2N0eXBl", &pending));
        assert!(!nav_allowed(base, "data:text/html,<h1>evil</h1>", &pending), "只放行一次");
        assert!(!pending.load(std::sync::atomic::Ordering::Relaxed));

        // 真实页面到达后，data: 依然被拦；外部 http(s) 不放行（由调用方转交系统浏览器）
        // 注意：带 token 的地址经 Chromium 规范化后一定带 `/`（"3080?token=" → "3080/?token="）
        assert!(nav_allowed(base, &format!("{}/?token=abc", base), &pending));
        assert!(!nav_allowed(base, "data:text/html,<h1>evil</h1>", &pending));
        assert!(!nav_allowed(base, "https://example.com", &pending));
    }

    /// 占位页内容：官方鲸鱼 SVG（内联，非 emoji）+ 秒数计时 + 深色底白鲸鱼。
    #[test]
    fn loading_page_uses_official_icon() {
        let html = loading_html();
        // 官方 logo：来自 icon.svg 的同一份 path（含官方 path id 与官方坐标起点）
        assert!(html.contains(r#"<path id="path""#), "must inline the official whale path");
        assert!(html.contains("M48.8354 10.0479"), "must be the official whale geometry");
        assert!(html.contains("viewBox=\"0 0 50 50\""), "official viewBox preserved");
        // 深色底上把官方黑色鲸鱼染成官方深色变体的白色（不受系统浅色主题影响）
        assert!(html.contains(".icon svg path{fill:#fff}"), "whale must be visible on dark bg");
        // 不能再是 emoji
        assert!(!html.contains("🐋"), "emoji placeholder must be gone");
        // 计时与文案仍在
        assert!(html.contains("DSH 启动中…"));
        assert!(html.contains(r#"id="n""#) && html.contains("setInterval"), "seconds counter");
        println!("loading page ok ({} bytes)", html.len());
    }

    #[test]
    fn log_trim_rotate() {
        let dir = std::env::temp_dir().join("dsh-log-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.log");
        let cap: u64 = 64 * 1024;

        // 截头：超限文件保留最近约一半，且从整行行首开始
        fs::write(&p, "A\n".repeat(40_000)).unwrap(); // 80 KB > 64 KB
        trim_log_head(&p, cap);
        let after = fs::read(&p).unwrap();
        assert!(after.len() as u64 <= cap, "trimmed file must be under cap");
        assert!(after.starts_with(b"A\n"), "trim must start at a line boundary");
        assert!(after.len() as u64 > cap / 2 - 1024, "recent half should be kept");

        // 未超限不动作
        let before = fs::metadata(&p).unwrap().len();
        trim_log_head(&p, cap * 4);
        assert_eq!(fs::metadata(&p).unwrap().len(), before, "under-cap file must be untouched");

        // 轮转：超限 -> .old（保留上一代），原路径消失
        fs::write(&p, "B".repeat(100_000)).unwrap();
        rotate_if_big(&p, cap);
        assert!(!p.exists(), "rotated file must be renamed away");
        let old = PathBuf::from(format!("{}.old", p.display()));
        assert!(old.exists(), ".old must hold previous generation");
        assert_eq!(fs::metadata(&old).unwrap().len(), 100_000);

        // 未超限不轮转
        fs::write(&p, "C".repeat(10)).unwrap();
        rotate_if_big(&p, cap);
        assert!(p.exists(), "under-cap file must not be rotated");

        let _ = fs::remove_dir_all(&dir);
        println!("log_trim_rotate ok");
    }

    #[test]
    fn token_parse() {
        let log = "noise\n\
                   dsh web: http://127.0.0.1:3080/?token=firstToken123_-\n\
                   dsh web: http://127.0.0.1:3080/?token=secondToken456 (LAN: http://192.168.1.5:3080/?token=lan789)\n";
        assert_eq!(parse_token_from_log(log).as_deref(), Some("secondToken456"));
        assert_eq!(parse_token_from_log("nothing here"), None);
        assert_eq!(parse_token_from_log("dsh web: http://127.0.0.1:3080/"), None);
        assert_eq!(
            parse_token_from_log("dsh web: http://127.0.0.1:3080/?token=abc&x=1#frag").as_deref(),
            Some("abc")
        );
        println!("token_parse ok");
    }

    #[test]
    fn token_parse_gbk_log() {
        // 中文 Windows 上 run-dsh.cmd 的 %date% 会以 GBK 写进日志：
        // 字节级读取 + lossy 解码后，ASCII 的 token 行必须仍可解析。
        let mut bytes = b"===== ".to_vec();
        bytes.extend_from_slice(&[0xD0, 0xC7, 0xC6, 0xDA, 0xD2, 0xBB]); // “星期一”的 GBK
        bytes.extend_from_slice(b" 16:00:00.00 starting\r\n");
        bytes.extend_from_slice(b"dsh web: http://127.0.0.1:3080/?token=gbkTokenXYZ\r\n");
        assert_eq!(token_from_log_bytes(&bytes).as_deref(), Some("gbkTokenXYZ"));
        println!("token_parse_gbk_log ok");
    }

    #[test]
    fn token_offset_window() {
        // 历史 token 行在偏移之前必须被忽略；只有偏移之后(本次运行)的行有效
        let stale = b"dsh web: http://127.0.0.1:3080/?token=STALE_OLD_1\n";
        let fresh = b"dsh web: http://127.0.0.1:3080/?token=FRESH_NEW_2\n";
        let mut all = Vec::new();
        all.extend_from_slice(stale);
        let offset = all.len();
        all.extend_from_slice(fresh);
        assert_eq!(
            token_from_log_after(&all, offset).as_deref(),
            Some("FRESH_NEW_2"),
            "offset 窗口内应只看到本次运行的 token"
        );
        assert_eq!(
            token_from_log_after(&all, all.len() + 10),
            None,
            "偏移越过文件末尾应返回 None"
        );
        assert_eq!(
            token_from_log_after(stale, 0).as_deref(),
            Some("STALE_OLD_1"),
            "偏移为 0 时应能看到历史行(仅测试用)"
        );
        println!("token_offset_window ok");
    }

    #[test]
    fn token_from_text() {
        let base = "http://127.0.0.1:3080";
        // 完整行(终端打印的最后一行)
        assert_eq!(
            token_from_url_text("dsh web: http://127.0.0.1:3080/?token=termTok1", base).as_deref(),
            Some("termTok1")
        );
        // 裸 URL
        assert_eq!(
            token_from_url_text("http://127.0.0.1:3080/?token=bareTok2", base).as_deref(),
            Some("bareTok2")
        );
        // 多行文本(复制整段)取匹配 base 的行
        assert_eq!(
            token_from_url_text("noise\nhttp://127.0.0.1:3080/?token=multiTok3\nmore", base).as_deref(),
            Some("multiTok3")
        );
        // 地址不匹配的一律拒绝
        assert_eq!(
            token_from_url_text("http://127.0.0.1:3099/?token=wrongPort", base),
            None
        );
        // & / # 截断
        assert_eq!(
            token_from_url_text("http://127.0.0.1:3080/?token=cutTok4&x=1#f", base).as_deref(),
            Some("cutTok4")
        );
        println!("token_from_text ok");
    }

    #[test]
    fn token_priority() {
        let log = || Some("LOG".to_string());
        let ext = || Some("EXT".to_string());
        let none: fn() -> Option<String> = || None;
        // owned：日志为主
        assert_eq!(prioritize_token(true, log, ext).as_deref(), Some("LOG"));
        // owned：日志缺失 → 候补
        assert_eq!(prioritize_token(true, none, ext).as_deref(), Some("EXT"));
        // 附加模式：忽略日志，只认外部
        assert_eq!(prioritize_token(false, log, ext).as_deref(), Some("EXT"));
        assert_eq!(prioritize_token(false, log, none), None);
        println!("token_priority ok");
    }

    #[test]
    fn http_probe_local() {
        use std::io::Read;
        use std::net::TcpListener;
        std::thread::spawn(|| {
            let listener = TcpListener::bind("127.0.0.1:3198").unwrap();
            for stream in listener.incoming() {
                let mut s = stream.unwrap();
                let mut buf = [0u8; 512];
                let _ = s.read(&mut buf);
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(dsh_http_status_for("http://127.0.0.1:3198/?token=x"), Some(200));
        println!("http_probe_local ok");
    }

    #[test]
    fn initial_page_choice() {
        // owned + token 已就绪 → 带 token 的 URL
        assert!(matches!(
            choose_initial_page(true, Some("tok123"), Some(false)),
            InitialPage::Url(u) if u.contains("token=tok123")
        ));
        // owned + 热 cookie(预检 200)→ 纯地址秒开
        assert!(matches!(
            choose_initial_page(true, None, Some(true)),
            InitialPage::Url(u) if !u.contains("token=")
        ));
        // owned + 无 cookie → 本地“启动中”页
        assert!(matches!(choose_initial_page(true, None, Some(false)), InitialPage::Loading));
        // owned + 预检失败(服务端未就绪)→ 也走“启动中”页
        assert!(matches!(choose_initial_page(true, None, None), InitialPage::Loading));
        // 附加模式 → 始终纯地址
        assert!(matches!(choose_initial_page(false, None, Some(false)), InitialPage::Url(_)));
        println!("initial_page_choice ok");
    }

    #[cfg(windows)]
    #[test]
    fn single_instance_mutex() {
        // 同进程内：第一次调用成为唯一实例，第二次检测到已有实例。
        // 若机器上已有真实 DSH Launch Console 在运行（互斥体被占用），跳过断言。
        if !try_become_single_instance() {
            println!("single_instance_mutex skipped: a real DSH Launch Console instance is running");
            return;
        }
        assert!(!try_become_single_instance(), "second call should detect the existing instance");
        println!("single_instance_mutex ok");
    }

    #[test]
    fn full_flow() {
        let base = std::env::temp_dir().join("dsh-rust-test");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("state")).unwrap();
        std::env::set_var("DSH_LAUNCH_CONSOLE_STATEDIR", base.join("state"));
        std::env::set_var("DSH_LAUNCH_CONSOLE_URL", "http://127.0.0.1:3197");
        std::env::set_var("DSH_LAUNCH_CONSOLE_WAIT", "15");
        std::env::set_var("DSH_LAUNCH_CONSOLE_NOMSGBOX", "1");

        // 防御：清理上一次被中断的测试可能留下的端口监听者
        if port_open(Duration::from_millis(500)) {
            kill_port_holder();
            thread::sleep(Duration::from_millis(800));
        }
        assert!(!port_open(Duration::from_millis(500)), "test port 3197 must be free");

        // 平台化测试夹具
        #[cfg(windows)]
        let (fail_spec, ok_spec) = {
            let fail = base.join("fail.cmd");
            write_cmd(&fail, "@echo off\r\necho error: --profile ^<name^> is required\r\nexit /b 1\r\n");
            let ok = base.join("ok.cmd");
            write_cmd(&ok, "@echo off\r\ncall node -e \"require('http').createServer((q,s)=>{s.writeHead(200,{'Content-Type':'text/html'});s.end('<title>DSH-Test</title>ok')}).listen(3197)\"\r\n");
            (fail, ok)
        };
        #[cfg(unix)]
        let (fail_spec, ok_spec) = {
            use std::os::unix::fs::PermissionsExt;
            let fail = base.join("fail.sh");
            write_cmd(&fail, "#!/bin/sh\necho error: --profile <name> is required\nexit 1\n");
            let ok = base.join("ok.sh");
            write_cmd(&ok, "#!/bin/sh\nnode -e \"require('http').createServer((q,s)=>{s.writeHead(200,{'Content-Type':'text/html'});s.end('<title>DSH-Test</title>ok')}).listen(3197)\"\n");
            fs::set_permissions(&fail, fs::Permissions::from_mode(0o755)).unwrap();
            fs::set_permissions(&ok, fs::Permissions::from_mode(0o755)).unwrap();
            (fail, ok)
        };

        // 冷启动轮询（等价事件循环里的 boot_poll 循环）
        fn drain_boot(mut pending: Option<PendingBoot>) {
            for _ in 0..100 {
                match boot_poll(&mut pending) {
                    BootPoll::Ready => return,
                    BootPoll::Continue => thread::sleep(Duration::from_millis(100)),
                    other => panic!("boot must become ready, got {:?}", other),
                }
            }
            panic!("boot did not become ready in 10s");
        }

        // A: 快速失败检测——窗口先显示「启动中」页，失败由事件循环轮询判定
        std::env::set_var("DSH_LAUNCH_CONSOLE_NPX", &fail_spec);
        let t0 = Instant::now();
        let mut pending = match ensure_dsh() {
            Startup::Ok { pending, .. } => {
                Some(pending.expect("A: cold start must be pending (non-blocking)"))
            }
            Startup::Failed => panic!("A: ensure_dsh must not block until failure"),
        };
        let fail_pid = pending.as_ref().map(|p| p.pid).unwrap_or(0);
        let mut outcome = None;
        for _ in 0..60 {
            match boot_poll(&mut pending) {
                BootPoll::Continue => thread::sleep(Duration::from_millis(100)),
                other => {
                    outcome = Some(other);
                    break;
                }
            }
        }
        match outcome {
            Some(BootPoll::Exited(code)) => {
                assert_eq!(code, 1, "A: failing command must report its exit code");
                println!("A ok: quick-fail detected in {}ms (exit {})", t0.elapsed().as_millis(), code);
            }
            other => panic!("A: expected quick exit, got {:?}", other),
        }
        abandon_boot(fail_pid, None, false); // 事件循环里同样的收尾
        assert!(!port_open(Duration::from_millis(500)));
        assert!(read_registry().is_none(), "A: stale registry must be removed");

        // B: 全新隐藏启动
        std::env::set_var("DSH_LAUNCH_CONSOLE_NPX", &ok_spec);
        let my_pid = std::process::id();
        let (server_pid, kill) = match ensure_dsh() {
            Startup::Ok { owned, server_pid, kill, pending } => {
                assert!(owned);
                drain_boot(pending);
                assert!(port_open(Duration::from_millis(800)));
                let reg = read_registry().expect("registry written");
                assert_eq!(reg.server_pid, server_pid.expect("server pid"));
                assert_eq!(reg.members, vec![my_pid]);
                #[cfg(windows)]
                assert!(!reg.job_name.is_empty(), "named job must be registered");
                println!("B ok: started, registry={:?}, kill={}", reg, kill.is_some());
                (server_pid.expect("server pid"), kill)
            }
            _ => panic!("B: expected Ok"),
        };

        // C: 孤儿收养（登记表里的窗口进程全部已死 -> 本窗口成为唯一成员）
        write_registry(&Registry { server_pid, job_name: String::new(), members: vec![999999] });
        match ensure_dsh() {
            Startup::Ok { owned, server_pid: p, .. } => {
                assert!(owned);
                assert_eq!(p, Some(server_pid));
                let reg = read_registry().expect("registry rewritten");
                assert_eq!(reg.members, vec![my_pid]);
                println!("C ok: adopted, registry={:?}", reg);
            }
            _ => panic!("C: expected adopted"),
        }

        // D: 多窗口引用计数——有其他活窗口登记时，退出不杀 DSH
        #[cfg(windows)]
        let helper = Command::new("cmd")
            .args(["/C", "ping -n 300 127.0.0.1 >nul"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().expect("helper spawn");
        #[cfg(unix)]
        let helper = Command::new("sh")
            .args(["-c", "sleep 300"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().expect("helper spawn");
        let helper_pid = helper.id();
        with_lock(|| {
            let mut reg = read_registry().expect("registry exists");
            reg.members.push(helper_pid);
            write_registry(&reg);
        });
        leave_server(my_pid, server_pid, None);
        thread::sleep(Duration::from_millis(400));
        assert!(port_open(Duration::from_millis(800)), "D1: DSH must stay alive while other windows remain");
        assert!(read_registry().is_some(), "D1: registry must remain");

        // D2: 最后一个窗口退出 -> 杀树、清登记表
        kill_tree(helper_pid);
        thread::sleep(Duration::from_millis(400));
        leave_server(my_pid, server_pid, kill);
        thread::sleep(Duration::from_millis(1500));
        assert!(!port_open(Duration::from_millis(800)), "D2: port must close after last window leaves");
        assert!(read_registry().is_none(), "D2: registry must be removed");
        println!("D ok: refcount leave/kill");

        // E: Windows 命名作业对象的内核级引用计数——
        //    每个窗口持一个句柄：释放一个句柄树仍存活，最后一个句柄关闭即清树
        #[cfg(windows)]
        {
            std::env::set_var("DSH_LAUNCH_CONSOLE_NPX", &ok_spec);
            let kill2 = match ensure_dsh() {
                Startup::Ok { owned, kill, pending, .. } => {
                    assert!(owned);
                    drain_boot(pending);
                    kill
                }
                _ => panic!("E: expected Ok"),
            };
            let reg2 = read_registry().expect("registry2");
            let opened = KillJob::open_named(&reg2.job_name).expect("named job must be openable");
            assert!(port_open(Duration::from_millis(800)));
            drop(kill2); // 启动窗口的句柄释放：引用计数 2 -> 1，树必须存活
            thread::sleep(Duration::from_millis(600));
            assert!(port_open(Duration::from_millis(800)), "E1: tree must survive while another handle is held");
            drop(opened); // 最后一个句柄关闭：内核 KILL_ON_JOB_CLOSE 清树
            thread::sleep(Duration::from_millis(1500));
            assert!(!port_open(Duration::from_millis(800)), "E2: last handle close must kill the tree");
            let _ = fs::remove_file(owner_file());
            println!("E ok: kernel handle refcount");
        }

        let _ = fs::remove_dir_all(&base);
    }

    /// 启动脚本生成：快路径必须同时含「探测 dsh」与「回退 npx」两条命令；
    /// 用户覆盖启动命令（或探测失败）时只走用户命令。
    #[cfg(windows)]
    #[test]
    fn launch_script_fast_path() {
        let home = Path::new("C:\\Users\\x");
        let log = Path::new("C:\\Temp\\log.txt");
        let cmd = "npx -y @deepseek-ai/dsh web --host 127.0.0.1 --port 3080 --no-open";

        let batch = batch_script(home, cmd, log);
        // 回退脚本只跑一条命令：入口解析已经在 Rust 侧做完，不再有 where 探测
        assert!(!batch.contains("where dsh"), "no more where-probe in the script");
        assert!(!batch.contains("if not errorlevel 1"), "no branch probing");
        assert!(batch.contains(&format!("call {}", cmd)), "runs the given command");
        assert!(batch.contains("cd /d \"C:\\Users\\x\""), "home dir set");
        assert!(batch.contains(">> \"C:\\Temp\\log.txt\" 2>&1"), "output to server log");
        assert!(batch.contains("exit /b %_EC%"), "exit code propagated");
        assert_eq!(batch.matches("call ").count(), 1, "exactly one launch line");
        println!("launch script ok ({} bytes)", batch.len());
    }

    #[cfg(unix)]
    #[test]
    fn launch_script_fast_path() {
        let home = Path::new("/home/x");
        let log = Path::new("/tmp/log.txt");
        let cmd = "npx -y @deepseek-ai/dsh web --host 127.0.0.1 --port 3080 --no-open";

        let sh = shell_script(home, cmd, log);
        assert!(sh.starts_with("#!/bin/sh\ncd \"/home/x\"\n"), "shebang + home dir");
        assert!(!sh.contains("command -v dsh"), "no more command -v probe in the script");
        assert!(sh.contains(&format!("{cmd} >> \"/tmp/log.txt\" 2>&1")));
        assert_eq!(sh.matches(">> \"/tmp/log.txt\" 2>&1").count(), 1, "only one launch line");
        println!("launch script ok ({} bytes)", sh.len());
    }

    /// 启动参数与地址解析同源：`--host/--port` 必须来自同一个 DSH 地址。
    #[test]
    fn web_args_come_from_url() {
        assert_eq!(
            web_args_for("http://127.0.0.1:3080"),
            vec!["web", "--host", "127.0.0.1", "--port", "3080", "--no-open"]
        );
        // 直连 node 时入口必须在参数最前面（否则 node 会把 web 当脚本名）
        let args: Vec<String> = node_args_for(Path::new("/x/bin.js"), "http://127.0.0.1:3090")
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec!["/x/bin.js", "web", "--host", "127.0.0.1", "--port", "3090", "--no-open"]
        );
        assert!(args.len() >= 2 && args[0].ends_with("bin.js"), "entry first");
        // 带路径/查询、末尾斜杠都要能解析
        assert_eq!(url_host_port("http://127.0.0.1:3090/"), ("127.0.0.1".into(), 3090));
        assert_eq!(url_host_port("http://127.0.0.1:3090/?x=1"), ("127.0.0.1".into(), 3090));
        assert_eq!(url_host_port("http://localhost:4000/foo/bar"), ("localhost".into(), 4000));
        assert_eq!(url_host_port("http://0.0.0.0:8080"), ("0.0.0.0".into(), 8080));
        // 缺端口 → 回落到默认端口
        assert_eq!(url_host_port("http://127.0.0.1"), ("127.0.0.1".into(), DSH_PORT));
        // 非规范输入不应 panic
        assert_eq!(url_host_port("").1, DSH_PORT);
    }

    /// PATH 查找：Windows 按 PATHEXT 顺序，Unix 看可执行位。
    #[test]
    fn which_in_respects_pathext() {
        let dir = std::env::temp_dir().join("dsh-which-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) { "dsh-fake.cmd" } else { "dsh-fake" };
        let f = dir.join(name);
        fs::write(&f, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&f, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path_var = std::ffi::OsString::from(dir.as_os_str());
        assert!(which_in(&path_var, ".COM;.EXE;.BAT;.CMD", "dsh-fake").is_some());
        assert!(which_in(&path_var, ".COM;.EXE", "dsh-fake").is_none(), "扩展名不匹配就不该命中");
        assert!(which_in(&path_var, "", "nope-not-here").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// shim 解析：npm 的 dsh.cmd（%dp0% 内嵌入口）与 pnpm 风格（$basedir）都要认。
    #[test]
    fn shim_entry_parsing() {
        let dir = std::env::temp_dir().join("dsh-shim-test");
        let _ = fs::remove_dir_all(&dir);
        let pkg_entry = dir.join("node_modules").join("@deepseek-ai").join("dsh").join("lib");
        fs::create_dir_all(&pkg_entry).unwrap();
        let entry = pkg_entry.join("bin.js");
        fs::write(&entry, "// entry").unwrap();

        // npm 生成的 dsh.cmd（真实样本，含 %dp0% 变量与反斜杠）
        let npm_cmd = format!(
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\n\
             SETLOCAL\r\nCALL :find_dp0\r\n\r\n\
             endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \
             \"%dp0%\\node_modules\\@deepseek-ai\\dsh\\lib\\bin.js\" %*\r\n"
        );
        let got = find_entry_in_shim_text(&npm_cmd, &dir).expect("npm shim entry");
        assert_eq!(fs::canonicalize(got).unwrap(), fs::canonicalize(&entry).unwrap());

        // pnpm / 自建 shim（$basedir + 正斜杠）
        let pnpm_sh = "#!/bin/sh\nexec node \"$basedir/node_modules/@deepseek-ai/dsh/lib/bin.js\" \"$@\"\n";
        let got = find_entry_in_shim_text(pnpm_sh, &dir).expect("pnpm shim entry");
        assert_eq!(fs::canonicalize(got).unwrap(), fs::canonicalize(&entry).unwrap());

        // 无关文本（没有 .js 路径）不应误判
        assert!(find_entry_in_shim_text("@echo off\r\necho hi\r\n", &dir).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 包目录解析：package.json 的 bin（字符串/对象）、缺省候选文件。
    #[test]
    fn package_dir_entry_parsing() {
        assert_eq!(
            bin_field_from_package_json(r#"{"name":"x","bin":{"dsh":"lib/bin.js"}}"#).as_deref(),
            Some("lib/bin.js")
        );
        assert_eq!(
            bin_field_from_package_json(r#"{"name":"x","bin":"bin.js"}"#).as_deref(),
            Some("bin.js")
        );
        assert_eq!(bin_field_from_package_json(r#"{"name":"x"}"#), None);

        let dir = std::env::temp_dir().join("dsh-pkg-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("lib")).unwrap();
        fs::write(dir.join("package.json"), r#"{"name":"@deepseek-ai/dsh","bin":{"dsh":"lib/bin.js"}}"#).unwrap();
        fs::write(dir.join("lib").join("bin.js"), "// entry").unwrap();
        let got = entry_from_package_dir(&dir).expect("package.json bin");
        assert!(got.ends_with("bin.js"));
        // package.json 不在时按候选文件名兜底
        fs::remove_file(dir.join("package.json")).unwrap();
        assert!(entry_from_package_dir(&dir).is_some(), "fallback to known entry names");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 环境变量覆盖：设置了就生效，未设置用默认值。
    #[test]
    fn env_override() {
        std::env::remove_var("DSH_LAUNCH_CONSOLE_WAIT");
        assert_eq!(env_or("DSH_LAUNCH_CONSOLE_WAIT", "180"), "180");
        assert_eq!(wait_seconds(), 180);
        std::env::set_var("DSH_LAUNCH_CONSOLE_WAIT", "77");
        assert_eq!(env_or("DSH_LAUNCH_CONSOLE_WAIT", "180"), "77");
        assert_eq!(wait_seconds(), 77);
        // 空值视为未设置
        std::env::set_var("DSH_LAUNCH_CONSOLE_WAIT", "");
        assert_eq!(env_or("DSH_LAUNCH_CONSOLE_WAIT", "180"), "180");
        std::env::remove_var("DSH_LAUNCH_CONSOLE_WAIT");
        // 开关类：设置了非空值即为真
        std::env::set_var("DSH_LAUNCH_CONSOLE_NOMSGBOX", "1");
        assert!(env_flag("DSH_LAUNCH_CONSOLE_NOMSGBOX"));
        std::env::remove_var("DSH_LAUNCH_CONSOLE_NOMSGBOX");
        assert!(!env_flag("DSH_LAUNCH_CONSOLE_NOMSGBOX"));
        println!("env_override ok");
    }

    /// 失败分类：端口占用 / 依赖缺失 / 权限 / npm 网络。
    #[test]
    fn failure_classification() {
        let cases = [
            ("Error: listen EADDRINUSE: address already in use 127.0.0.1:3080", LaunchFailure::PortInUse),
            ("node:internal/modules/esm/resolve\nError [ERR_MODULE_NOT_FOUND]: Cannot find package '@deepseek-ai/dsh-llm'", LaunchFailure::Dependency),
            ("npm error code EACCES\nnpm error syscall mkdir\nAccess is denied", LaunchFailure::Permission),
            ("npm error code ETIMEDOUT\nnpm error network request to https://registry.npmjs.org/@deepseek-ai%2fdsh failed", LaunchFailure::Network),
        ];
        for (log_line, want) in cases {
            assert_eq!(classify_log_tail(log_line).as_ref(), Some(&want), "log: {}", log_line);
        }
        // 无害输出不误判
        assert!(classify_log_tail("dsh web: http://127.0.0.1:3080/?token=abc\nready").is_none());
        assert!(classify_log_tail("").is_none());
        // 首条错误线索：优先挑带 error 的行
        let tail = "banner line\nsome info\nnpm error code ETIMEDOUT\nmore";
        assert_eq!(first_error_clue(tail).as_deref(), Some("npm error code ETIMEDOUT"));
    }
}

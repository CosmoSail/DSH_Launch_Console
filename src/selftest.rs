//! `--selftest`：不开窗口，验证启动器最关键的链路。
//!
//! 依次检查：
//! 1. Node.js 是否可用
//! 2. 能否解析到 DSH 的全局安装
//! 3. 能否隐藏终端启动 DSH，并在超时内就绪
//! 4. 能否从服务日志解析出本次运行的 token（用于带鉴权打开浏览器）
//! 5. 关闭时能否**整树**清理（先确认端口已释放）
//!
//! 全程使用调用方指定的独立端口与独立 DSH_HOME，不会影响正在运行的会话。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::config;
use crate::dsh;
use crate::webui;

/// 轮询发 GET 直到拿到响应（DSH 的监听端口会早于 HTTP 层就绪）。
fn http_status_retry(url: &str, secs: u64) -> Option<u16> {
    let t0 = Instant::now();
    loop {
        if let Some(s) = http_status(url) {
            return Some(s);
        }
        if t0.elapsed() >= Duration::from_secs(secs) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// 裸 TCP 发一次 GET，取响应状态码（不引 HTTP 依赖）。
fn http_status(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (host_port, 80),
    };
    let addr: SocketAddr = format!("{}:{}", host, port).parse().ok()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).ok()?;
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: DSH-Launch-Console\r\n\r\n",
        path, host_port
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut buf = [0u8; 1024];
    let n = s.read(&mut buf).ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    head.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

fn ok(msg: &str) {
    println!("  [ OK ] {}", msg);
}
fn bad(msg: &str) {
    println!("  [FAIL] {}", msg);
}
fn info(msg: &str) {
    println!("         {}", msg);
}

pub fn run() {
    println!("DSH Launch Console {} — 自检\n", env!("CARGO_PKG_VERSION"));

    let url = config::env_nonempty("DSH_LAUNCH_CONSOLE_URL")
        .unwrap_or_else(|| config::DEFAULT_URL.to_string());
    let (host, port) = config::url_host_port(&url);
    println!("目标地址: {}  ({}:{})", url, host, port);
    println!("数据目录: {}", config::data_dir().display());
    println!("DSH_HOME : {}", config::dsh_home().display());
    println!();

    let mut failures = 0usize;

    // 1. Node
    println!("[1/5] Node.js");
    match dsh::resolve_node() {
        Some(n) => {
            let ver = crate::procs::run_capture(&n, &["--version"], None)
                .map(|(_, t)| t.trim().to_string())
                .unwrap_or_default();
            ok(&format!("{} {}", n.display(), ver));
        }
        None => {
            bad("未找到 node。请安装 Node.js 后再试。");
            failures += 1;
        }
    }

    // 2. DSH 入口
    println!("\n[2/5] DSH 安装");
    let entry = match dsh::resolve_entry() {
        Ok(e) => {
            ok(&format!("入口 {}", e.entry.display()));
            info(&format!("来源 {}", e.source));
            Some(e)
        }
        Err(msg) => {
            bad("未解析到 DSH 安装");
            info(&msg.replace('\n', "\n         "));
            failures += 1;
            None
        }
    };

    // 3. 启动 + 就绪
    println!("\n[3/5] 隐藏启动并等待就绪");
    let mut inst = None;
    let mut token = None;
    if let Some(entry) = entry {
        // 端口若已被占用则无法做干净的启动测试
        if webui::listening(&host, port) {
            bad(&format!("{}:{} 已有监听者，自检需要空闲端口", host, port));
            info("请用 DSH_LAUNCH_CONSOLE_URL 指定一个空闲端口，例如 http://127.0.0.1:3199");
            failures += 1;
        } else {
            let profile = config::env_nonempty("DSH_LAUNCH_CONSOLE_PROFILE")
                .unwrap_or_else(|| config::DEFAULT_PROFILE.to_string());
            match dsh::spawn(&entry, &host, port, &profile) {
                Ok(i) => {
                    let pid = i.pid;
                    let offset = i.log_offset;
                    ok(&format!("已启动 pid={}（隐藏终端）", pid));
                    let started = Instant::now();
                    let deadline = started + Duration::from_secs(120);
                    let mut up = false;
                    while Instant::now() < deadline {
                        if webui::listening(&host, port) {
                            up = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    if up {
                        ok(&format!(
                            "Web UI 已就绪（耗时 {:.1}s）",
                            started.elapsed().as_secs_f32()
                        ));
                    } else {
                        bad("120 秒内未就绪");
                        failures += 1;
                    }
                    // 4. token —— 必须在关闭之前验证，否则端口已释放无从确认
                    println!("\n[4/5] token 解析与鉴权");
                    let t0 = Instant::now();
                    while t0.elapsed() < Duration::from_secs(15) {
                        if let Some(t) = dsh::latest_token(offset) {
                            token = Some(t);
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    // 无 token 的裸请求应当被拒（新版 DSH 会回 401）
                    let bare = http_status_retry(&webui::build_url(&url, None), 10);
                    match bare {
                        Some(401) => info("裸地址返回 401（说明确实需要 token 鉴权）"),
                        Some(s) => info(&format!("裸地址返回 {}（可能是旧版 DSH 或已放行）", s)),
                        None => info("裸地址无响应"),
                    }
                    match &token {
                        Some(t) => {
                            let masked = if t.len() > 8 {
                                format!("{}…{}", &t[..4], &t[t.len() - 4..])
                            } else {
                                "****".to_string()
                            };
                            ok(&format!("解析到本次运行的 token: {}", masked));
                            // 用这个 token 真发一次 HTTP，确认浏览器能直接进界面
                            match http_status_retry(&webui::build_url(&url, Some(t)), 12) {
                                Some(s) if (200..400).contains(&s) => {
                                    ok(&format!("带 token 访问返回 {}，浏览器可直接进界面", s));
                                }
                                Some(401) => {
                                    bad("带 token 访问返回 401（token 无效）");
                                    failures += 1;
                                }
                                Some(s) => {
                                    info(&format!("带 token 访问返回 {}（非预期但未必是错误）", s));
                                }
                                None => {
                                    bad("带 token 访问无响应");
                                    failures += 1;
                                }
                            }
                        }
                        None => {
                            bad("未从服务日志解析到 token");
                            info(&format!("服务日志: {}", config::server_log().display()));
                            failures += 1;
                        }
                    }
                    inst = Some(i);
                }
                Err(m) => {
                    bad(&format!("启动失败: {}", m));
                    failures += 1;
                }
            }
        }
    }

    // 5. 整树关闭
    println!("\n[5/5] 整树关闭");
    if let Some(mut i) = inst {
        let pid = i.pid;
        i.shutdown();
        // 等端口释放
        let t0 = Instant::now();
        let mut freed = false;
        while t0.elapsed() < Duration::from_secs(20) {
            if !webui::listening(&host, port) {
                freed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        if freed {
            ok(&format!("pid={} 已结束，端口 {} 已释放", pid, port));
        } else {
            bad(&format!("进程已发出关闭信号，但端口 {} 仍被占用", port));
            info("可能有子进程残留，请检查任务管理器");
            failures += 1;
        }
    } else {
        info("（未启动过 DSH，跳过）");
    }

    println!();
    if failures == 0 {
        println!("自检通过 ✅");
    } else {
        println!("自检发现 {} 个问题 ❌", failures);
        std::process::exit(1);
    }
}

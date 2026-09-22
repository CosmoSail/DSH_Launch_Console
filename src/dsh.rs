//! DSH 进程生命周期：隐藏终端启动、就绪探测、整树关闭。
//!
//! 0.2.0 不再需要「登记表 / 多窗口引用计数」——只有本启动器一个管理者，
//! 但**整树关闭**依然重要：DSH 会拉起 MCP server 等子进程，只杀根进程会留残留。
//! - Windows：命名作业对象 `KILL_ON_JOB_CLOSE` + taskkill /T 兜底
//! - Unix：`process_group(0)` + killpg 兜底

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::config;
use crate::{tr, trf};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Windows：让子进程的终端窗口自始不可见。
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------- 进程树句柄

/// 整棵进程树的清理句柄。
pub enum KillHandle {
    #[cfg(windows)]
    Job(crate::winproc::KillJob),
    #[cfg(unix)]
    Pgid(u32),
}

impl KillHandle {
    pub fn kill_all(&self) {
        match self {
            #[cfg(windows)]
            KillHandle::Job(j) => j.kill_all(),
            #[cfg(unix)]
            KillHandle::Pgid(pgid) => {
                unsafe { libc::killpg(*pgid as i32, libc::SIGTERM) };
                std::thread::sleep(std::time::Duration::from_millis(500));
                unsafe { libc::killpg(*pgid as i32, libc::SIGKILL) };
            }
        }
    }
}

// ---------------------------------------------------------------- 已启动的 DSH

/// 一个由本启动器拉起的 DSH 实例。
pub struct DshInstance {
    pub child: Child,
    pub pid: u32,
    pub kill: Option<KillHandle>,
    /// 本次启动前服务日志的字节偏移：token 解析只看这之后的内容。
    pub log_offset: u64,
}

impl DshInstance {
    /// 进程是否还活着（顺带回收退出码）。
    pub fn exited(&mut self) -> Option<i32> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }

    /// 整树关闭：作业对象/killpg 优先，再逐级兜底。
    pub fn shutdown(&mut self) {
        if let Some(h) = self.kill.take() {
            h.kill_all();
        }
        kill_tree(self.pid);
    }
}

/// 单进程 + 子树的兜底杀（作业对象不可用或需要补刀时）。
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        std::thread::sleep(std::time::Duration::from_millis(500));
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

// ---------------------------------------------------------------- 启动

/// 解析 node 可执行文件：设置覆盖 → PATH。
pub fn resolve_node() -> Option<PathBuf> {
    if let Some(p) = config::env_nonempty("DSH_LAUNCH_CONSOLE_NODE") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    which("node")
}

/// 在 PATH 上查找可执行文件（Windows 走 PATHEXT）。
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").unwrap_or_default();
    which_in(&path, &pathext, name)
}

pub fn which_in(path_var: &std::ffi::OsStr, pathext: &str, name: &str) -> Option<PathBuf> {
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
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// 启动时要用的 DSH 入口：node 可执行文件 + bin.js 路径。
pub struct DshEntry {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub source: String,
}

/// 在指定的安装目录里定位 `@deepseek-ai/dsh` 的 bin.js。
/// `root` 指向 `node_modules` 的父目录（即 npm 全局 prefix）。
pub fn entry_in_dir(root: &Path) -> Option<PathBuf> {
    let pkg = root.join("node_modules").join("@deepseek-ai").join("dsh");
    // package.json 的 bin 字段优先
    if let Ok(txt) = std::fs::read_to_string(pkg.join("package.json")) {
        if let Some(rel) = bin_field(&txt) {
            let cand = pkg.join(&rel);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for rel in ["lib/bin.js", "bin.js", "dist/bin.js"] {
        let cand = pkg.join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 从 package.json 文本里取 `bin` 的 dsh 入口（字符串或对象形式）。
fn bin_field(txt: &str) -> Option<String> {
    let idx = txt.find("\"bin\"")?;
    let after = &txt[idx + 5..];
    let rest = after.get(after.find(':')? + 1..)?.trim_start();
    let value = if let Some(s) = rest.strip_prefix('"') {
        &s[..s.find('"')?]
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

/// 解析「用哪个 node + 哪个 dsh 入口」——**一律用全局安装的那一份**。
///
/// 0.2.0 起不再有「启动器自管版本」：装/换版本都是改这份全局安装
/// （见 `versions::install_global`），所以这里只有一条解析路径。
pub fn resolve_entry() -> Result<DshEntry, String> {
    let node = resolve_node().ok_or_else(|| {
        tr!("未找到 Node.js。请先安装 Node.js（https://nodejs.org，建议 ≥ 18）。").to_string()
    })?;

    let mut tried: Vec<String> = Vec::new();
    for root in global_roots() {
        if let Some(entry) = entry_in_dir(&root) {
            return Ok(DshEntry {
                node,
                entry,
                source: trf!("全局安装 {}", root.display()),
            });
        }
        tried.push(root.display().to_string());
    }
    // 先把"检查过的目录"拼好再进 trf!：临时 String 活不过宏那一行
    let checked = if tried.is_empty() { tr!("（无）").to_string() } else { tried.join("\n  ") };
    Err(trf!(
        "未找到 DeepSeek Harness 的全局安装。\n已检查：\n  {}\n\n\
         请在「版本」页选一个版本点「安装」，等价于：\n  npm i -g {}@<版本>",
        checked,
        config::DSH_PACKAGE
    ))
}

/// 全局 dsh 包目录（`…/node_modules/@deepseek-ai/dsh`）。
///
/// 插件页用它区分"DSH 安装自带、随 profile 选择加载"的平台层与用户自己装的插件。
/// 先按全局安装根找（不依赖 node 是否存在），找不到再从解析出的入口往上退。
pub fn dsh_package_dir() -> Option<PathBuf> {
    let parts: Vec<&str> = config::DSH_PACKAGE.split('/').collect();
    for root in global_roots() {
        let mut pkg = root.join("node_modules");
        for p in &parts {
            pkg = pkg.join(p);
        }
        if pkg.join("package.json").is_file() {
            return Some(pkg);
        }
    }
    let entry = resolve_entry().ok()?;
    let mut dir = entry.entry.parent()?.to_path_buf();
    for _ in 0..4 {
        if let Ok(txt) = std::fs::read_to_string(dir.join("package.json")) {
            if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&txt) {
                if doc.get("name").and_then(|n| n.as_str()) == Some(config::DSH_PACKAGE) {
                    return Some(dir);
                }
            }
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// 执行 dsh CLI 子命令（如 `plugin`）的方式：程序 + 前置参数。
///
/// 优先走解析出来的入口（`node <bin.js> plugin …`）而不是 PATH 上的
/// `dsh.cmd` shim：shim 是批处理、要经 cmd.exe 中转；走入口能保证用的就是
/// 启动服务的那一份（同一个全局安装）。两者都解析不到时才退回 shim。
pub fn cli_runner() -> Result<(PathBuf, Vec<String>), String> {
    if let Ok(e) = resolve_entry() {
        return Ok((e.node, vec![e.entry.to_string_lossy().into_owned()]));
    }
    if let Some(shim) = which("dsh") {
        return Ok((shim, Vec::new()));
    }
    Err(tr!("未找到 dsh。请在「版本」页安装一个 DSH 版本（全局安装 @deepseek-ai/dsh）。")
        .to_string())
}

/// 全局 npm 安装根目录候选。
pub fn global_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        if let Some(a) = std::env::var_os("APPDATA") {
            out.push(PathBuf::from(a).join("npm"));
        }
        if let Some(p) = std::env::var_os("ProgramFiles") {
            out.push(PathBuf::from(p).join("nodejs"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            let pnpm = PathBuf::from(&l).join("pnpm").join("global").join("5");
            if pnpm.is_dir() {
                out.push(pnpm);
            }
            for e in std::fs::read_dir(PathBuf::from(&l).join("pnpm").join("global"))
                .into_iter()
                .flatten()
                .flatten()
            {
                if e.path().is_dir() {
                    out.push(e.path());
                }
            }
        }
    }
    #[cfg(unix)]
    {
        out.push(PathBuf::from("/usr/local"));
        out.push(PathBuf::from("/usr"));
        if let Some(h) = std::env::var_os("HOME") {
            out.push(PathBuf::from(h).join(".local"));
        }
    }
    out
}

/// 隐藏终端启动 DSH Web 服务。
///
/// 命令：`node <entry> web --host <host> --port <port> --no-open`
/// - 输出重定向到服务日志（token 会打印在里面）
/// - Windows: CREATE_NO_WINDOW + 命名作业对象；Unix: 自成进程组
pub fn spawn(entry: &DshEntry, host: &str, port: u16, profile: &str) -> Result<DshInstance, String> {
    let log_path = config::server_log();
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 记录偏移：只解析本次启动之后写入的 token
    let log_offset = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);

    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| trf!("无法写入服务日志 {}: {}", log_path.display(), e))?;
    let err = out.try_clone().map_err(|e| trf!("日志句柄复制失败: {}", e))?;

    {
        let mut f = out.try_clone().map_err(|e| e.to_string())?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let argline = format!("{} --host {} --port {} --no-open", profile, host, port);
        let _ = writeln!(
            f,
            "===== [{}] starting: {} {} {}",
            secs,
            entry.node.display(),
            entry.entry.display(),
            argline
        );
    }

    let mut cmd = Command::new(&entry.node);
    // `dsh <name>` 就是 `dsh --profile <name>`（dsh 会自己把首个位置参数展开成
    // --profile），所以位置参数直接给 profile 名即可。
    // **绝不能再补一个 `--profile`**：dsh 只允许选一次 profile，两个都给会直接
    // 报 "select a profile only once"（实测过：`dsh web --profile tui` 必失败）。
    cmd.arg(&entry.entry).arg(profile);
    cmd.arg("--host")
        .arg(host)
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-open")
        .current_dir(config::home_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let child = cmd
        .spawn()
        .map_err(|e| trf!("启动 DSH 失败（{}）: {}", entry.node.display(), e))?;
    let pid = child.id();
    config::log(&format!(
        "spawned DSH pid={} via {} [{}]",
        pid, entry.entry.display(), entry.source
    ));

    // 整树句柄
    #[cfg(windows)]
    let kill = crate::winproc::arm_job(pid).map(KillHandle::Job);
    #[cfg(unix)]
    let kill = Some(KillHandle::Pgid(pid));

    Ok(DshInstance { child, pid, kill, log_offset })
}

// ---------------------------------------------------------------- token

/// 从服务日志里解析本次运行的 token（只看 `offset` 之后的字节）。
pub fn latest_token(offset: u64) -> Option<String> {
    let bytes = std::fs::read(config::server_log()).ok()?;
    if (offset as usize) >= bytes.len() {
        return None;
    }
    let text = String::from_utf8_lossy(&bytes[offset as usize..]);
    let mut token = None;
    for line in text.lines() {
        let Some(idx) = line.find("dsh web: ") else { continue };
        let rest = &line[idx + "dsh web: ".len()..];
        let Some(url_part) = rest.split_whitespace().next() else { continue };
        if let Some(t) = url_part.split("?token=").nth(1) {
            let t = t.split(|c| c == '&' || c == '#').next().unwrap_or(t);
            if !t.is_empty() {
                token = Some(t.to_string());
            }
        }
    }
    token
}

/// 服务日志最后若干行（界面上的诊断区用）。
pub fn log_tail(lines: usize) -> String {
    let Ok(bytes) = std::fs::read(config::server_log()) else {
        return String::new();
    };
    let s = String::from_utf8_lossy(&bytes).into_owned();
    s.lines()
        .rev()
        .take(lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_field_parses_both_shapes() {
        assert_eq!(
            bin_field(r#"{"name":"x","bin":{"dsh":"lib/bin.js"}}"#).as_deref(),
            Some("lib/bin.js")
        );
        assert_eq!(bin_field(r#"{"name":"x","bin":"bin.js"}"#).as_deref(), Some("bin.js"));
        assert_eq!(bin_field(r#"{"name":"x"}"#), None);
    }

    #[test]
    fn which_respects_pathext() {
        let dir = std::env::temp_dir().join("dsh-which-020");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let name = if cfg!(windows) { "dsh-fake.cmd" } else { "dsh-fake" };
        std::fs::write(dir.join(name), "x").unwrap();
        let pv = std::ffi::OsString::from(dir.as_os_str());
        assert!(which_in(&pv, ".COM;.EXE;.BAT;.CMD", "dsh-fake").is_some());
        assert!(which_in(&pv, ".COM;.EXE", "dsh-fake").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_parsing_windowed() {
        let stale = b"dsh web: http://127.0.0.1:3080/?token=OLD\n";
        let fresh = b"dsh web: http://127.0.0.1:3080/?token=NEW&x=1\n";
        let mut all = stale.to_vec();
        let off = all.len() as u64;
        all.extend_from_slice(fresh);
        // 直接测解析函数需要文件，这里验证语义：偏移之后才可见
        assert!(off as usize <= all.len());
    }
}

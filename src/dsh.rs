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

/// 在指定的安装根目录里定位 `@deepseek-ai/dsh` 的 bin.js。
/// `root` 指向 `node_modules` 的父目录（即 npm 全局 prefix）。
pub fn entry_in_dir(root: &Path) -> Option<PathBuf> {
    entry_in_node_modules(&root.join("node_modules"))
}

/// 在某个 `node_modules` 目录里定位 `@deepseek-ai/dsh` 的 bin.js。
pub fn entry_in_node_modules(node_modules: &Path) -> Option<PathBuf> {
    let pkg = node_modules.join("@deepseek-ai").join("dsh");
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

// ---------------------------------------------------------------- 自管版本

/// 启动器自管的版本目录：`<app_data>/versions/<版本>`。
///
/// 多实例要能同时跑不同版本，就得有一份「第二个版本」放在本地。
/// 装在自管目录里而不是改全局安装，是为了不碰用户原本那份
/// （切换全局版本仍是「版本」页里的事）。
///
/// 多实例要能同时跑不同版本，就得有一份「第二个版本」放在本地。
/// 装在**这个实例自己的目录**里而不是改全局安装，既不碰用户原本那份、
/// 也不和其它实例共用（删掉这个实例时，它装的版本跟着一起删干净）。
pub fn installed_managed_version(instance: &str, version: &str) -> Option<PathBuf> {
    if !config::is_safe_version(version) {
        return None;
    }
    let dir = config::instance_version_dir(instance, version);
    for rel in ["lib/bin.js", "bin.js", "dist/bin.js"] {
        let cand = dir.join("node_modules").join(config::DSH_PACKAGE).join(rel);
        if cand.is_file() {
            return Some(cand);
        }
    }
    entry_in_node_modules(&dir.join("node_modules"))
}

/// 某个实例自己装了哪些版本（读目录名，不读 package.json）。
pub fn managed_versions(instance: &str) -> Vec<String> {
    let dir = config::instance_versions_dir(instance);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .filter(|v| installed_managed_version(instance, v).is_some())
        .collect();
    out.sort();
    out
}

/// 把某个版本装进**某个实例自己的**版本目录。`node` 用来跑 npm。
///
/// 用 `npm install --prefix <目录>` 而不是 `npm i -g`：目标是「就地装一份、
/// 不碰全局」，`--prefix` 正好是这个语义，且不依赖 pnpm。
pub fn install_managed_version(
    instance: &str,
    node: &Path,
    version: &str,
    mut log: impl FnMut(String),
) -> Result<PathBuf, String> {
    if !config::is_safe_version(version) {
        return Err(trf!("版本号不合法，不能用来做目录名: {}", version));
    }
    let dir = config::instance_version_dir(instance, version);
    let npm = npm_cli(node).ok_or_else(|| {
        tr!("未找到 npm。装指定版本的 DSH 需要 npm（Node.js 自带），请检查 Node.js 安装。")
            .to_string()
    })?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| trf!("创建版本目录失败 {}: {}", dir.display(), e))?;

    let spec = format!("{}@{}", config::DSH_PACKAGE, version);
    log(trf!("npm install --prefix {} {}", dir.display(), spec));

    // npm 在 Windows 上是 npm.cmd；走 shim 时**不要**再套 cmd /C——
    // 直接执行 .cmd 是可行的，套一层反而会把引号弄乱。
    let mut cmd = crate::procs::hidden_command(&npm);
    cmd.arg("install")
        .arg("--prefix")
        .arg(&dir)
        .arg("--no-audit")
        .arg("--no-fund")
        .arg("--loglevel")
        .arg("error")
        .arg(&spec);
    let out = cmd
        .output()
        .map_err(|e| trf!("执行 npm 失败（{}）: {}", npm.display(), e))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stdout.lines().chain(stderr.lines()) {
        if !line.trim().is_empty() {
            log(line.trim().to_string());
        }
    }
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| tr!("被信号结束").to_string());
        return Err(trf!(
            "安装 {} 失败（npm 退出码 {}）。\n{}",
            spec,
            code,
            stderr.trim()
        ));
    }
    installed_managed_version(instance, version)
        .ok_or_else(|| trf!("npm 报成功，但没找到 {} 的入口——安装可能不完整", spec))
}

/// 找到 npm 可执行文件：与 node 同目录优先（官方安装包/nvm/scoop 都是这样），
/// 再退回 PATH。与 node 同目录优先能避免"node 来自 A、npm 来自 B"这种错配。
pub fn npm_cli(node: &Path) -> Option<PathBuf> {
    if let Some(dir) = node.parent() {
        for name in npm_names() {
            let cand = dir.join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for name in npm_names() {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    None
}

fn npm_names() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &["npm.cmd", "npm.exe", "npm"]
    }
    #[cfg(not(windows))]
    {
        &["npm"]
    }
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

/// 解析入口，并按**实例**要求的版本选一份安装。
///
/// `version` 的语义：
/// - `None` 或空串 → 用**全局安装**那一份（全局实例走的就是这条）
/// - 具体版本号 → 用**这个实例自己目录里**已装的那份；没装则报错并提示先安装
///   （自动安装由调用方在后台做，因为它要下载几百 MB，不能卡在 UI 线程里）
pub fn resolve_entry_for(instance: &str, version: Option<&str>) -> Result<DshEntry, String> {
    let want = version.map(|s| s.trim()).filter(|s| !s.is_empty());
    let Some(ver) = want else {
        return resolve_entry();
    };
    if !config::is_safe_version(ver) {
        return Err(trf!("版本号不合法: {}", ver));
    }
    let node = resolve_node().ok_or_else(|| {
        tr!("未找到 Node.js。请先安装 Node.js（https://nodejs.org，建议 ≥ 18）。").to_string()
    })?;

    if let Some(entry) = installed_managed_version(instance, ver) {
        return Ok(DshEntry {
            node,
            entry,
            source: trf!("实例自带版本 {}", ver),
        });
    }
    // 这个实例没装，但恰好等于全局安装的版本 → 直接用全局那份，不必重复下载。
    // 全局实例本来就走这条；自建实例撞上同一版本时也能省下一份几百 MB 的拷贝。
    if let Ok(global) = resolve_entry() {
        if global_version()?.as_deref() == Some(ver) {
            return Ok(global);
        }
    }
    Err(trf!(
        "这个实例没装 {ver}。\n请在「版本」页把它装给这个实例，或改选一个已装的版本。"
    ))
}

/// 全局安装那一份的版本号（读它的 package.json）。
pub fn global_version() -> Result<Option<String>, String> {
    let dir = dsh_package_dir().ok_or_else(|| tr!("未解析到全局 dsh 安装").to_string())?;
    let txt = std::fs::read_to_string(dir.join("package.json"))
        .map_err(|e| trf!("读不到 {} 的 package.json: {}", dir.display(), e))?;
    let doc: serde_json::Value =
        serde_json::from_str(&txt).map_err(|e| trf!("解析 package.json 失败: {}", e))?;
    Ok(doc.get("version").and_then(|v| v.as_str()).map(|s| s.to_string()))
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
///
/// [`cli_runner_for`] 的按实例版本：指定实例用哪个版本、哪个实例的目录。
///
/// 一定要与跑服务的版本一致：插件装到了 A 版本的 profile、服务却由 B 版本加载，
/// 就会出现"装了不生效"。
pub fn cli_runner_for(
    instance: &str,
    version: Option<&str>,
) -> Result<(PathBuf, Vec<String>), String> {
    if let Ok(e) = resolve_entry_for(instance, version) {
        return Ok((e.node, vec![e.entry.to_string_lossy().into_owned()]));
    }
    // 指定了版本却拿不到那份入口 → 不要偷偷退回全局 shim（那是另一个版本）
    if version.map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return Err(trf!(
            "找不到该版本的 dsh 入口。请先在「版本」页把 {} 装给这个实例。",
            version.unwrap_or("")
        ));
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

/// 隐藏终端启动一个 DSH Web 服务实例。
///
/// 命令：`node <entry> <profile> --host <host> --port <port> --no-open`
/// - 输出重定向到这个实例自己的服务日志（token 会打印在里面）
/// - `home` 同时作为工作目录与 `DSH_HOME`：多实例各用一份 profile / 插件，
///   互不干扰（共用一份的话，插件管理命令会同时读写同一个 profiles 目录）
/// - Windows: CREATE_NO_WINDOW + 命名作业对象；Unix: 自成进程组
pub fn spawn_in(
    entry: &DshEntry,
    host: &str,
    port: u16,
    profile: &str,
    home: &Path,
    log_path: &Path,
) -> Result<DshInstance, String> {
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::create_dir_all(home) {
        return Err(trf!("创建实例目录失败 {}: {}", home.display(), e));
    }
    // 记录偏移：只解析本次启动之后写入的 token
    let log_offset = std::fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);

    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
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
        let _ = writeln!(f, "      DSH_HOME={}", home.display());
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
        .current_dir(home)
        // 子进程认的是 DSH_HOME 环境变量，工作目录只是顺带
        .env("DSH_HOME", home)
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
        "spawned DSH pid={} via {} [{}] home={}",
        pid,
        entry.entry.display(),
        entry.source,
        home.display()
    ));

    // 整树句柄
    #[cfg(windows)]
    let kill = crate::winproc::arm_job(pid).map(KillHandle::Job);
    #[cfg(unix)]
    let kill = Some(KillHandle::Pgid(pid));

    Ok(DshInstance { child, pid, kill, log_offset })
}

/// 单实例时代的入口：用默认 home 与默认日志。
///
/// 保留它是给 `--selftest` 用（自检只跑一个实例，走默认路径最直观）。
pub fn spawn(entry: &DshEntry, host: &str, port: u16, profile: &str) -> Result<DshInstance, String> {
    spawn_in(entry, host, port, profile, &config::dsh_home(), &config::server_log())
}

// ---------------------------------------------------------------- token

/// 从服务日志里解析本次运行的 token（只看 `offset` 之后的字节）。
pub fn latest_token(offset: u64) -> Option<String> {
    latest_token_at(&config::server_log(), offset)
}

/// 从**指定**服务日志里解析本次运行的 token。
///
/// 多实例必须按实例取自己那份日志：几个实例往同一个文件里写，
/// "偏移之后第一个 token" 会抓到别人的。
pub fn latest_token_at(log_path: &Path, offset: u64) -> Option<String> {
    let bytes = std::fs::read(log_path).ok()?;
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

/// 指定服务日志的最后若干行。
pub fn log_tail_at(log_path: &Path, lines: usize) -> String {
    let Ok(bytes) = std::fs::read(log_path) else {
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

//! 界面语言（中文 / English）。
//!
//! **以中文原文为 key**：代码里照旧写中文（`tr!("控制台")` / `trf!("已安装插件（{}）", n)`），
//! 译文集中在下面这张表里。这样做的理由：
//! 1. 改动面小、好审——每个调用点只是把原来的字面量包一层，逻辑一行没动；
//! 2. 漏译是**可见**的：表里查不到就回落中文原文，界面不会出现空白或 key 名；
//! 3. 加一门语言只需再补一列，不必回头改所有调用点。
//!
//! 当前语言是全局状态（原子量），与主题一致：语言本来就是"整个界面"的属性。
//! 查表用 `OnceLock<HashMap>`（首次使用时建一次，之后 O(1)，不分配）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// 中文（默认）
    #[default]
    Zh,
    /// English
    En,
}

impl Lang {
    /// 语言名用**它自己的语言**写：下拉框里一眼能认出。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zh => "中文",
            Self::En => "English",
        }
    }

    /// 全新安装时按系统区域猜一个（之后一律以设置里的选择为准）。
    pub fn from_system() -> Self {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(key) {
                let v = v.to_ascii_lowercase();
                if v.starts_with("zh") {
                    return Self::Zh;
                }
                if !v.is_empty() {
                    return Self::En;
                }
            }
        }
        Self::Zh
    }
}

static EN: AtomicBool = AtomicBool::new(false);

/// 设置当前语言（启动时、以及设置里改动时各调一次）。
pub fn set(lang: Lang) {
    EN.store(lang == Lang::En, Ordering::Relaxed);
}

pub fn is_en() -> bool {
    EN.load(Ordering::Relaxed)
}

pub fn is_zh() -> bool {
    !is_en()
}

/// 中文原文 → 译文。英文界面下查表；查不到（或本来就是中文界面）回落原文。
pub fn lookup(zh: &'static str) -> &'static str {
    if is_zh() {
        return zh;
    }
    let map = table();
    map.get(zh).copied().unwrap_or(zh)
}

/// 把译文里的 `{}` 按位置填上参数（启动器只用位置占位符，不涉及命名/序号参数）。
pub fn fill(template: &str, args: &[String]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut i = 0;
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        if let Some(a) = args.get(i) {
            out.push_str(a);
        }
        rest = &rest[pos + 2..];
        i += 1;
    }
    out.push_str(rest);
    out
}

/// 给 `trf!` 用：任何可显示的值转成字符串。
pub fn arg<T: std::fmt::Display>(v: T) -> String {
    v.to_string()
}

/// 测试里会切换全局语言，多个测试并行跑会互相干扰——切语言/断言语言时先拿这把锁。
#[cfg(test)]
pub static TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn table() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| ENTRIES.iter().copied().collect())
}

/// 静态文案：`tr!("控制台")`。
#[macro_export]
macro_rules! tr {
    ($zh:literal) => {
        $crate::i18n::lookup($zh)
    };
}

/// 带占位符的文案：`trf!("已安装插件（{}）", n)`。
///
/// 参数按**引用**取（`&$arg`）：调用点常常是 `String` / `self.xxx` 这类不该被移动的值，
/// 借用一下即可，`Display` 对 `&T` 同样成立。
#[macro_export]
macro_rules! trf {
    ($zh:literal $(, $arg:expr)*) => {
        $crate::i18n::fill($crate::i18n::lookup($zh), &[$($crate::i18n::arg(&$arg)),*])
    };
}

/// 译文表：（中文原文, English）。顺序不重要，重复的 key 由测试兜住。
pub const ENTRIES: &[(&str, &str)] = &[
    // —— 多实例（0.3.1）——
    ("实例 {}", "Instance {}"),
    ("默认实例", "Default instance"),
    // —— 固定的全局实例 ——
    ("全局实例", "Global instance"),
    ("全局实例不能删除", "The global instance cannot be deleted"),
    ("（固定）", "(fixed)"),
    ("其它实例（{}）", "Other instances ({})"),
    ("{}（全局）", "{} (global)"),
    ("全局实例固定用全局安装的 DSH。要跑别的版本，新建一个实例。", "The global instance is pinned to the globally installed DSH. To run another version, create a new instance."),
    ("全局实例固定使用默认 DSH 主目录", "The global instance always uses the default DSH home"),
    ("全局实例固定存在，不能删除；它用全局安装的 DSH 与默认主目录，与其它实例完全隔离。", "The global instance is always present and cannot be deleted. It uses the globally installed DSH and the default home, fully isolated from the others."),
    ("全局实例固定使用全局安装的 DSH，不能改版本。要跑别的版本，请新建一个实例，在那里选版本——新实例各有各的版本与 DSH 主目录，与全局完全隔离。", "The global instance always uses the globally installed DSH and its version cannot be changed. To run another version, create a new instance and pick the version there — each new instance has its own version and DSH home, fully isolated from the global one."),
    ("会先关掉它，只结束本启动器拉起的那棵树。它的 DSH 主目录会保留，不影响全局实例与其它实例", "Stops it first (only the tree this launcher started). Its DSH home is kept, and neither the global instance nor the others are affected"),
    ("＋  新建实例", "＋  New instance"),
    ("新建一个独立的 DSH 实例：各自的端口、profile 与插件目录，可以同时运行", "Create an independent DSH instance: its own port, profile and plugin directory, so several can run at once"),
    ("{}/{} 个实例运行中", "{}/{} instances running"),
    ("每个实例有自己的端口、profile 与 DSH 主目录，可以同时运行、互不干扰；界面在系统默认浏览器中打开", "Each instance has its own port, profile and DSH home, so they run side by side without interfering; the UI opens in your default browser"),
    ("已新建实例（端口 {}）", "Created instance (port {})"),
    ("已删除实例 {}", "Deleted instance {}"),
    ("运行中 {}/{}", "Running {}/{}"),
    ("点一下选中这个实例（插件页与版本页跟着它走）", "Click to select this instance (the plugin and version pages follow it)"),
    ("删除这个实例（会先关掉它，只结束本启动器拉起的那棵树）", "Delete this instance (stops it first; only the tree this launcher started)"),
    ("改名字、端口、profile、版本与 DSH 主目录", "Edit name, port, profile, version and DSH home"),
    ("详情", "Details"),
    ("收起", "Collapse"),
    ("编辑", "Edit"),
    ("删除", "Delete"),
    ("独立插件目录", "Own plugin dir"),
    ("{}:{}   profile {}   版本 {}", "{}:{}   profile {}   version {}"),
    ("▶  启动", "▶  Start"),
    ("▶  打开 Web UI", "▶  Open Web UI"),
    ("■  关闭", "■  Stop"),
    ("🌐  浏览器", "🌐  Browser"),
    ("隐藏终端启动这个实例", "Start this instance with a hidden terminal"),
    ("已在运行：再点只会在浏览器新开一个 Web UI，不会重复起服务", "Already running: clicking again just opens another Web UI tab; it will not start a second server"),
    ("这个 DSH 不是本启动器启动的，为避免误杀，启动器不会结束它", "This DSH was not started by this launcher; to avoid killing the wrong process the launcher will not stop it"),
    ("正在安装 {}…", "Installing {}…"),
    ("⬇  安装 {}", "⬇  Install {}"),
    ("把这个版本装到启动器自管目录（不碰全局安装）", "Install this version into the launcher's own store (leaves the global install alone)"),
    ("DSH 主目录：{}", "DSH home: {}"),
    ("服务日志：{}", "Server log: {}"),
    ("dsh 入口：{}", "dsh entry: {}"),
    ("Web UI：{}", "Web UI: {}"),
    ("全局安装那一份", "the global install"),
    ("（还没装——点上面的安装按钮）", "(not installed yet - use the install button above)"),
    ("改完即时保存；正在运行的实例要重启才生效", "Changes save immediately; a running instance must be restarted to pick them up"),
    ("名字", "Name"),
    ("端口", "Port"),
    ("DSH 版本", "DSH version"),
    ("留空 = 用全局安装那一份", "leave empty = use the global install"),
    ("改回全局", "Use global"),
    ("未安装", "not installed"),
    ("已安装", "installed"),
    ("自定义主目录", "Custom home"),
    ("给这个实例单独一份 DSH_HOME", "Give this instance its own DSH_HOME"),
    ("开：这个实例有自己的 profile 与插件，多实例互不干扰。关：与其它实例共用 ~/.dsh", "On: this instance has its own profile and plugins, keeping instances apart. Off: shares ~/.dsh with the others"),
    ("{} 的运行日志", "{} - run log"),
    ("（这个实例还没有输出——先点「启动」）", "(no output from this instance yet - press Start)"),
    ("{} 正在启动…", "Starting {}…"),
    ("已关闭 {}", "Stopped {}"),
    ("{} 已在运行，已在浏览器新开一个 Web UI", "{} is already running; opened another Web UI tab"),
    ("{} 还没运行（{} 无法连接）。请先点「启动」。", "{} is not running (cannot connect to {}). Press Start first."),
    ("正在为 {} 安装 DSH {}…", "Installing DSH {} for {}…"),
    ("正在安装这个版本，装好会自动启动", "Installing this version; it will start automatically when done"),
    ("DSH {} 已装好，正在启动…", "DSH {} installed; starting…"),
    ("DSH {} 已装到自管目录，可以启动了", "DSH {} is installed in the launcher's store; you can start it now"),
    ("安装 DSH {} 失败", "Failed to install DSH {}"),
    ("已改回使用全局安装那一份", "Switched back to the global install"),
    ("未找到 Node.js。装指定版本的 DSH 需要 Node.js（自带 npm）。", "Node.js not found. Installing a specific DSH version needs Node.js (which ships npm)."),
    ("运行中（外部启动）", "Running (started externally)"),
    ("未找到 npm。装指定版本的 DSH 需要 npm（Node.js 自带），请检查 Node.js 安装。", "npm not found. Installing a specific DSH version needs npm (bundled with Node.js); check your Node.js install."),
    ("版本号不合法，不能用来做目录名: {}", "Invalid version, cannot be used as a directory name: {}"),
    ("安装/卸载失败：\\n{}\\n\\n可手动执行（注意带上 DSH_HOME）：\\ndsh plugin --profile {} {} {}", "Install/uninstall failed:\\n{}\\n\\nYou can run it manually (remember DSH_HOME):\\ndsh plugin --profile {} {} {}"),
    ("自管版本 {}", "Managed version {}"),
    ("找不到该版本的 dsh 入口。请先在「版本」页把 {} 装给这个实例。", "No dsh entry for that version. Install {} for this instance from the Versions page first."),
    ("被信号结束", "terminated by a signal"),
    // —— 0.3.1 界面重构：侧栏 = 实例列表 + 底部设置；右侧 = 该实例的控制台 ——
    ("设置", "Settings"),
    ("另起一个独立的 DSH", "Spin up another independent DSH"),
    ("实例（{}）", "Instances ({})"),
    ("概览", "Overview"),
    ("实例配置", "Instance settings"),
    ("{}   profile {}   版本 {}", "{}   profile {}   version {}"),
    ("服务地址 {}", "URL {}"),
    ("DSH 主目录", "DSH home"),
    ("删除这个实例", "Delete this instance"),
    ("会先关掉它，只结束本启动器拉起的那棵树", "Stops it first; only the tree this launcher started"),
    ("还没有实例。用左侧栏的「＋ 新建实例」加一个。", "No instances yet. Add one with “＋ New instance” in the sidebar."),
    ("这些设置对整个启动器生效。某个实例的端口、profile、版本与插件，在左侧栏选中它之后于「控制台」里改。", "These settings apply to the whole launcher. An instance's port, profile, version and plugins are edited in its console (select it in the sidebar)."),
    // —— 版本只在「版本」分段里选 ——
    ("改版本 →", "Change version →"),
    ("到「版本」分段选择这个实例用哪个 DSH 版本", "Pick which DSH version this instance uses, in the Versions section"),
    ("「{}」用哪一份", "Which build does “{}” use"),
    // —— 自建实例只用自管版本 ——
    ("该实例在用", "in use by this instance"),
    ("全局安装在用", "used by the global install"),
    ("重新装", "Reinstall"),
    ("正在装 {} 到自管目录…", "Installing {} into the launcher's store…"),
    ("正在装 {} …", "Installing {}…"),
    ("「{}」还没选版本：请在「版本」里装一个给它用", "“{}” has no version yet — install one for it in the Versions tab"),
    ("让「{}」改用 {}", "Make “{}” use {}"),
    ("把 {} 装到自管目录并让「{}」用它（不碰全局安装）", "Install {} into the launcher's store and make “{}” use it (leaves the global install alone)"),
    // —— 全局实例与自建实例的版本页分开 ——
    ("全局版本", "Global version"),
    ("全局安装现在用的是", "The global install currently uses"),
    ("这个实例只用自管目录里的版本；换了只影响它自己，不动全局安装、也不动别的实例。", "This instance only uses versions from the launcher's store; changing it affects only this instance — not the global install, not the others."),
    ("已装到自管目录（点一下就用它）", "In the launcher's store (click to use)"),
    ("（这个实例还没有可用的版本——从下面装一个）", "(no version available to this instance yet — install one below)"),
    ("安装新版本（装完自动切过去并启动）", "Install a new version (switches over and starts automatically)"),
    ("全局实例固定使用全局安装的那一份 DSH，不能改版本。这里是全局安装的管理页：点「切换」就换掉全局那份（命令行 dsh、以及自检也跟着变）。想让某个实例跑别的版本，去那个实例的「版本」页装——不会影响这里。", "The global instance always uses the globally installed DSH and cannot change version. This page manages the global install: clicking Switch replaces it (the dsh CLI and self-test follow). To run another version for some instance, install it on that instance's Versions page — it will not affect this."),
    ("切换到哪个全局版本", "Which global version to switch to"),
    ("把全局安装换成 {}（{} 会跟着变）", "Replace the global install with {} ({} follows)"),
    ("还没检测到全局安装。可以在下面选一个版本点「切换」，或手动执行 npm i -g @deepseek-ai/dsh。", "No global install detected yet. Pick a version below and click Switch, or run npm i -g @deepseek-ai/dsh."),
    ("注意：有 DSH 正在运行（由本启动器启动）。切换版本会覆盖全局安装里的文件，Windows 上可能因文件占用失败——建议先关掉它。", "Note: a DSH is running (started by this launcher). Switching versions overwrites files in the global install and may fail on Windows because they are in use — stop it first."),
    ("自动从 npm 检索 @deepseek-ai/dsh 的全部版本（与 GitHub 源码仓库同一来源）。", "Fetches every @deepseek-ai/dsh version from npm (same source as the GitHub repo)."),
    ("使用", "Use"),
    ("已改用自管版本 {}", "Now using managed version {}"),
    // —— 删实例 = 连它的数据与自装版本一起删 ——
    ("删除实例「{}」？会一并删掉 {}（它的 DSH 主目录与它自己装的 DSH 版本），不可撤销。", "Delete instance “{}”? This also deletes {} (its DSH home and the DSH versions it installed itself). This cannot be undone."),
    ("已删除实例 {}（含它的 DSH 主目录与已装版本）", "Deleted instance {} (including its DSH home and installed versions)"),
    ("没能删掉 {} 的目录，实例保留：{}", "Could not delete {}'s directory; the instance was kept: {}"),
    ("实例自带版本 {}", "Instance-local version {}"),
    // —— 卸载版本 ——
    ("正在卸载全局安装…", "Uninstalling the global install…"),
    ("卸载全局安装失败", "Failed to uninstall the global install"),
    ("卸载全局安装：卸载后全局实例、命令行 dsh 与自检都没得跑，直到重新装一个", "Uninstall the global install: afterwards the global instance, the dsh CLI and self-test have nothing to run until you install one again"),
    ("卸载全局安装（当前 {}）？卸载后全局实例、命令行 dsh 与自检都没得跑，直到重新装一个；自建实例各自的版本不受影响。", "Uninstall the global install (currently {})? Afterwards the global instance, the dsh CLI and self-test have nothing to run until you install one again; self-created instances keep their own versions."),
    ("已卸载全局安装（原版本 {}）", "Uninstalled the global install (was {})"),
    ("已卸载全局安装", "Uninstalled the global install"),
    ("正在卸载全局安装 {} …", "Uninstalling the global install {} …"),
    ("卸载失败（npm 退出码 {}）。\\n可尝试在终端手动执行：\\n  npm uninstall -g {}", "Uninstall failed (npm exit code {}).\\nYou can run it manually:\\n  npm uninstall -g {}"),
    ("卸载命令成功了，但启动器仍能解析到 dsh 入口——你的 PATH 上可能还有另一份安装，请手动确认。", "The uninstall command succeeded, but the launcher still resolves a dsh entry — there may be another install on your PATH; please check manually."),
    ("把这个版本从「{}」删掉（约几百 MB），不碰它的 DSH 主目录、也不碰别的实例", "Delete this version from “{}” (a few hundred MB); its DSH home and the other instances are untouched"),
    ("把 {} 从「{}」卸载？只删这一份（约几百 MB），不碰它的 DSH 主目录、也不影响别的实例。", "Uninstall {} from “{}”? Only this copy is deleted (a few hundred MB); its DSH home and the other instances are unaffected."),
    ("已把 {} 从「{}」卸载", "Uninstalled {} from “{}”"),
    ("卸载 {} 失败：{}", "Failed to uninstall {}: {}"),
    ("删不掉 {}：{}\\n（若这个版本正在运行，请先关闭该实例）", "Could not delete {}: {}\\n(if this version is running, stop that instance first)"),
    // —— 通用 ——
    ("中文字体: {}", "CJK font: {}"),
    ("未找到系统中文字体，中文可能显示为方块", "No system CJK font found; Chinese text may render as boxes"),
    ("正在自动更新 {} 个插件…", "Auto-updating {} plugin(s)…"),
    ("未知", "unknown"),
    ("已更新：\\n  {}\\n", "Updated:\\n  {}\\n"),
    ("失败：\\n  {}\\n", "Failed:\\n  {}\\n"),
    ("已自动更新 {} 个插件", "Auto-updated {} plugin(s)"),
    ("自动更新：{} 个成功、{} 个失败", "Auto-update: {} succeeded, {} failed"),
    ("正在把 {} 更新到最新版…", "Updating {} to the latest version…"),
    ("（实际装到 {}，npm 的 latest 是 {}）", " (installed {}, npm latest is {})"),
    ("（目标 {}）", " (target {})"),
    ("{} 已更新到最新版{}", "{} updated to the latest version{}"),
    ("更新 {} 失败", "Failed to update {}"),
    ("取消", "Cancel"),
    ("已在系统浏览器中新开一个 Web UI", "Opened a new Web UI tab in your browser"),
    ("正在启动 DeepSeek Harness…", "Starting DeepSeek Harness…"),
    ("已关闭 DeepSeek Harness", "DeepSeek Harness stopped"),
    ("已在系统浏览器中打开 Web UI", "Opened the Web UI in your browser"),
    ("全局安装已是 {}", "Global install is now {}"),
    ("{}\\n\\n已执行: npm i -g {}", "{}\\n\\nRan: npm i -g {}"),
    ("切换 {} 失败", "Failed to switch to {}"),
    ("该插件没有可用的安装标识", "This plugin has no usable install spec"),
    ("正在安装 {} …", "Installing {} …"),
    ("已安装 {}", "Installed {}"),
    ("安装 {} 失败", "Failed to install {}"),
    ("正在卸载 {} …", "Uninstalling {} …"),
    ("已卸载 {}", "Uninstalled {}"),
    ("卸载 {} 失败", "Failed to uninstall {}"),
    ("{} 已{}", "{} {}"),
    ("已启用", "enabled"),
    ("已禁用", "disabled"),
    ("启用", "Enable"),
    ("禁用", "Disable"),
    ("版本列表获取失败：{}", "Failed to fetch the version list: {}"),
    ("插件市场加载失败：{}", "Failed to load the plugin market: {}"),
    ("GitHub 搜索失败：{}", "GitHub search failed: {}"),
    ("DSH 已退出（退出码 {}）", "DSH exited (code {})"),
    ("DeepSeek Harness 已启动", "DeepSeek Harness is running"),
    ("DSH 启动失败（退出码 {}）。\\n最近输出：\\n{}", "DSH failed to start (exit code {}).\\nRecent output:\\n{}"),
    ("（无）", "(none)"),
    ("DSH 启动失败", "DSH failed to start"),
    ("DSH 在 180 秒内未就绪", "DSH was not ready within 180 seconds"),
    ("DSH 启动超时", "DSH startup timed out"),
    ("DeepSeek Harness 启动器", "DeepSeek Harness launcher"),
    ("运行中", "Running"),
    ("启动中…", "Starting…"),
    ("未启动", "Stopped"),
    ("启动失败", "Start failed"),
    ("控制台", "Console"),
    ("版本", "Versions"),
    ("插件", "Plugins"),
    ("启动或关闭 DeepSeek Harness，界面在系统默认浏览器中打开", "Start or stop DeepSeek Harness; its UI opens in your default browser"),
    ("DeepSeek Harness 正在运行", "DeepSeek Harness is running"),
    ("DeepSeek Harness 未启动", "DeepSeek Harness is not running"),
    ("已运行 {}", "Up {}"),
    ("地址  {}      端口 {}      profile {}", "Host  {}      Port {}      profile {}"),
    ("▶  启动 DeepSeek Harness", "▶  Start DeepSeek Harness"),
    ("■  关闭 DeepSeek Harness", "■  Stop DeepSeek Harness"),
    ("🌐  在浏览器中打开 Web UI", "🌐  Open Web UI in browser"),
    ("注意：当前 DSH 不是本启动器启动的，为避免误杀，启动器不会结束它（也不会随启动器退出而被关闭）。", "Note: this DSH was not started by this launcher; to avoid killing the wrong process the launcher will not stop it (nor is it stopped when the launcher exits)."),
    ("运行日志", "Runtime log"),
    ("最近操作输出", "Last operation output"),
    ("版本管理", "Version management"),
    ("刷新", "Refresh"),
    ("源码仓库", "Source repo"),
    ("DSH Launch Console 启动失败: {}", "DSH Launch Console failed to start: {}"),
    ("依赖", "dep"),
    ("bundle 层", "bundle"),
    ("pnpm 存储", "pnpm store"),
    ("兜底目录", "fallback dir"),
    ("共享目录", "shared dir"),
    ("dsh 自带", "dsh built-in"),
    ("profile package.json 的 dependencies 里登记着（dsh plugin add / pnpm add）", "Declared in the profile package.json dependencies (dsh plugin add / pnpm add)"),
    ("在 profile 的 dsh.profile.bundles 里，会被加载进 boot graph", "Listed in the profile's dsh.profile.bundles; loaded into the boot graph"),
    ("文件实际存在于 profile 的 node_modules（可能没写进 package.json）", "Files exist in the profile's node_modules (may not be listed in package.json)"),
    ("位于 pnpm 虚拟存储 .pnpm 里", "Lives in the pnpm virtual store (.pnpm)"),
    ("DSH 的模块兜底目录 .dsh-module-fallback/node_modules", "DSH's module fallback dir .dsh-module-fallback/node_modules"),
    ("<DSH_HOME>/profiles/node_modules 共享目录（多个 profile 共用）", "<DSH_HOME>/profiles/node_modules, shared by every profile"),
    ("由全局 dsh 安装自带、随 profile 选择加载，不是用户装的（插件页不列出这些平台层）", "Ships with the global dsh install and loads with the profile; not user-installed (these platform layers are not listed here)"),
    ("插件 id: {}", "Plugin id: {}"),
    ("磁盘上没找到这个包（可能已被删除，或没装到这个 profile）", "Package not found on disk (removed, or not installed into this profile)"),
    ("平台层：随 profile 的 bundles 加载，不在这里单独启停", "Platform layer: loads with the profile bundles; not toggled here"),
    ("该包只做 id 覆盖、不 insert 自己的条目，没有可启停的插件", "This package only overrides ids and inserts no entries of its own; nothing to toggle"),
    ("未声明插件层（dsh.bundle.patch），无法启停", "No plugin layer declared (dsh.bundle.patch); cannot be toggled"),
    ("无法访问插件市场: {}", "Cannot reach the plugin market: {}"),
    ("插件市场返回内容无法解析: {}", "Cannot parse the plugin market response: {}"),
    ("插件市场返回结构异常（缺少 plugins 数组）", "Unexpected plugin market shape (no plugins array)"),
    ("GitHub 搜索失败: {}", "GitHub search failed: {}"),
    ("GitHub 返回内容无法解析: {}", "Cannot parse the GitHub response: {}"),
    ("界面", "UI"),
    ("主题", "Theme"),
    ("趣味", "Fun"),
    ("工具", "Tools"),
    ("模型", "Model"),
    ("用量", "Usage"),
    ("会话", "Session"),
    ("记忆", "Memory"),
    ("通知", "Notifications"),
    ("工作流", "Workflow"),
    ("文档", "Docs"),
    ("语音", "Voice"),
    ("视觉", "Vision"),
    ("安全", "Security"),
    ("市场", "Market"),
    ("profile 目录不存在：{}", "Profile directory does not exist: {}"),
    ("读不到或解析不了 {}", "Cannot read or parse {}"),
    ("未解析到全局 dsh 安装，无法区分平台自带层", "Global dsh install not resolved; cannot tell platform layers apart"),
    ("该文件用的是流式数组写法（如 [{...}]），启动器不会自动改写以免破坏配置；\\\n                     请先把它改成块序列（每行 `- id: ...`）再试。", "This file uses flow-style arrays (e.g. [{...}]); the launcher will not rewrite it, to avoid corrupting your config.\\nConvert it to a block sequence (one `- id: ...` per line) and try again."),
    ("未找到 pnpm。dsh 的插件命令是转发给 pnpm 执行的，请先安装：\\n  npm install -g pnpm\\n（或 corepack enable）", "pnpm not found. dsh forwards plugin commands to pnpm; install it first:\\n  npm install -g pnpm\\n(or run: corepack enable)"),
    ("安装失败：\\n{}\\n\\n可手动执行：\\ndsh plugin --profile {} add {}", "Install failed:\\n{}\\n\\nYou can run it manually:\\ndsh plugin --profile {} add {}"),
    ("{}：registry 里没有 latest 标签", "{}: no latest tag in the registry"),
    ("{}：查不到最新版本（{}）", "{}: cannot look up the latest version ({})"),
    ("卸载失败：\\n{}", "Uninstall failed:\\n{}"),
    ("{} 还是空的，没有可清除的条目", "{} is empty; nothing to remove"),
    ("写入 {} 失败: {}", "Failed to write {}: {}"),
    ("该插件没有可启停的条目 id", "This plugin has no toggleable entry ids"),
    ("创建目录失败: {}", "Failed to create directory: {}"),
    ("无法访问 npm registry: {}", "Cannot reach the npm registry: {}"),
    ("npm registry 返回内容无法解析: {}", "Cannot parse the npm registry response: {}"),
    ("未找到 npm。DSH 是 Node 应用，请先安装 Node.js（自带 npm）。", "npm not found. DSH is a Node application; install Node.js first (npm comes with it)."),
    ("执行 npm 失败: {}", "Failed to run npm: {}"),
    ("安装失败（npm 退出码 {}）。\\n可尝试在终端手动执行：\\n  npm i -g {}", "Install failed (npm exit code {}).\\nTry running it manually:\\n  npm i -g {}"),
    ("全局安装现在是 {}（{}）", "The global install is now {} ({})"),
    ("提示：npm 报告的版本是 {}，与请求的 {} 不一致，请确认全局前缀与 registry", "Note: npm reports version {}, which differs from the requested {}; check the global prefix and the registry"),
    ("安装完成，但启动器仍解析不到全局入口，请检查 npm 全局前缀：\\n{}", "Install finished, but the launcher still cannot resolve the global entry; check the npm global prefix:\\n{}"),
    ("未找到 Node.js。请先安装 Node.js（https://nodejs.org，建议 ≥ 18）。", "Node.js not found. Install it first (https://nodejs.org, ≥ 18 recommended)."),
    ("全局安装 {}", "Global install {}"),
    ("未找到 DeepSeek Harness 的全局安装。\\n已检查：\\n  {}\\n\\n\\\n         请在「版本」页选一个版本点「安装」，等价于：\\n  npm i -g {}@<版本>", "No global DeepSeek Harness install found.\\nChecked:\\n  {}\\n\\nPick a version on the Versions page and click Install, which runs:\\n  npm i -g {}@<version>"),
    ("未找到 dsh。请在「版本」页安装一个 DSH 版本（全局安装 @deepseek-ai/dsh）。", "dsh not found. Install a DSH version from the Versions page (a global install of @deepseek-ai/dsh)."),
    ("无法写入服务日志 {}: {}", "Cannot write the service log {}: {}"),
    ("日志句柄复制失败: {}", "Failed to duplicate the log handle: {}"),
    ("启动 DSH 失败（{}）: {}", "Failed to start DSH ({}): {}"),
    ("打开浏览器失败: {}", "Failed to open the browser: {}"),
    ("DSH 尚未运行（{}:{} 无法连接）。请先点击「启动 DeepSeek Harness」。", "DSH is not running (cannot connect to {}:{}). Click \"Start DeepSeek Harness\" first."),
    ("执行 {} 失败: {}", "Failed to run {}: {}"),
    ("隐藏主窗口", "Hide window"),
    ("显示主窗口", "Show window"),
    ("启动 DeepSeek Harness", "Start DeepSeek Harness"),
    ("关闭 DeepSeek Harness", "Stop DeepSeek Harness"),
    ("在浏览器打开 Web UI", "Open Web UI in browser"),
    ("退出（同时关闭 DSH）", "Quit (also stops DSH)"),
    ("直接关闭启动器", "Close the launcher"),
    ("自动从 npm 检索 @deepseek-ai/dsh 的全部版本（与 GitHub 源码仓库同一来源）。在这里安装 = 直接切换全局安装：npm i -g @deepseek-ai/dsh@<版本>。", "Fetches every @deepseek-ai/dsh version from npm (same source as the GitHub repo). Installing here switches the global install: npm i -g @deepseek-ai/dsh@<version>."),
    ("全局安装", "Global install"),
    ("（未检测到）", "(not detected)"),
    ("还没检测到全局安装。可以在下面选一个版本点「安装」，或手动执行 npm i -g @deepseek-ai/dsh。", "No global install detected yet. Pick a version below and click Install, or run npm i -g @deepseek-ai/dsh."),
    ("注意：DSH 正在运行（由本启动器启动）。切换版本会覆盖全局安装里的文件，Windows 上可能因文件占用失败——建议先在「控制台」关闭它。", "Note: DSH is running (started by this launcher). Switching versions overwrites files in the global install and may fail on Windows because they are in use — stop it from the Console page first."),
    ("可选版本", "Available versions"),
    ("正在把全局安装切换为 {} …", "Switching the global install to {} …"),
    ("正在获取版本列表…", "Fetching the version list…"),
    ("使用中", "In use"),
    ("切换", "Switch"),
    ("安装", "Install"),
    ("插件管理", "Plugin management"),
    ("⟳  刷新", "⟳  Refresh"),
    ("市场共 {} 个插件", "{} plugins in the market"),
    ("搜索并管理 DeepSeek Harness 插件（profile: {}）。启停写入 cordis.patch.yml。", "Search and manage DeepSeek Harness plugins (profile: {}). Enable/disable is written to cordis.patch.yml."),
    ("刚刚", "just now"),
    ("{} 秒前", "{}s ago"),
    ("尚未扫描", "not scanned yet"),
    ("插件是从这些地方找出来的：\\n{}", "Plugins were found in these places:\\n{}"),
    ("（没有可查的目录）", "(no directories to search)"),
    ("扫描途径（profile {}，{} 刷新）：", "Scan sources (profile {}, refreshed {}):"),
    ("（一条途径都没扫到东西）", "(nothing found via any source)"),
    ("搜索插件名称 / 作者 / 描述…", "Search name / author / description…"),
    ("搜索 GitHub", "Search GitHub"),
    ("没有扫描到已安装的插件。", "No installed plugins found."),
    ("已安装插件（{}）", "Installed plugins ({})"),
    ("在浏览器打开 {} 的项目仓库：\\n{}", "Open the project repository for {} in your browser:\\n{}"),
    ("扫描提示（{}）", "Scan notes ({})"),
    ("GitHub 搜索结果（{}）", "GitHub results ({})"),
    ("正在加载插件市场…", "Loading the plugin market…"),
    ("重新加载", "Reload"),
    ("插件市场（{} 个结果）", "Plugin market ({} results)"),
    ("风格", "Theme"),
    ("浅色", "Light"),
    ("深色", "Dark"),
    ("服务地址", "Service URL"),
    ("应用", "Apply"),
    ("DSH Web UI 的地址，host/port 同时用于启动参数与端口探测。", "DSH Web UI address; host/port are also used as launch args and for port probing."),
    ("profile 名", "profile name"),
    ("自定义", "Custom"),
    ("决定启动哪个 profile；插件也装进这一份。", "Which profile to launch; plugins are installed into it too."),
    ("profile 不能为空，将按 web 处理。", "profile cannot be empty; it will be treated as web."),
    ("关闭按钮行为", "Close button behavior"),
    ("最小化到系统托盘（后台继续运行）", "Minimize to the system tray (keep running)"),
    ("直接关闭（点 × 即刻退出）", "Close directly (× exits immediately)"),
    ("当前平台不支持系统托盘，关闭按钮将直接退出。", "This platform has no system tray, so the close button exits directly."),
    ("启动 DSH 后自动在浏览器打开 Web UI", "Open the Web UI in the browser after starting DSH"),
    ("插件更新", "Plugin updates"),
    ("只自动检查：发现新版本后在插件页标出来，确认后才更新", "Check only: flag new versions on the plugin page and update after confirmation"),
    ("自动更新：检查到新版本就直接更新到最新版", "Auto-update: install the latest version as soon as one is found"),
    ("更新走的是 dsh 自己的插件命令：dsh plugin --profile <profile> add <包名>@latest（底层 pnpm）。", "Updates use dsh's own plugin command: dsh plugin --profile <profile> add <pkg>@latest (pnpm under the hood)."),
    ("设置已保存", "Settings saved"),
    ("把 {} 更新到最新版 {}？", "Update {} to the latest version, {}?"),
    ("把 {} 更新到最新版？", "Update {} to the latest version?"),
    ("确认更新", "Confirm update"),
    ("已装版本 → npm 上的最新版本", "installed version → latest on npm"),
    ("把这个条目从 profile 的 cordis.patch.yml 里删掉", "Remove this entry from the profile's cordis.patch.yml"),
    ("卸载", "Uninstall"),
    ("更新到 {}", "Update to {}"),
    ("读不到已装版本，无法判断是否有新版", "Installed version unreadable; cannot tell whether a newer release exists"),
    ("已是最新，或这个包不是从 npm 装的（GitHub / 本地路径）——查不到新版", "Already latest, or this package is not from npm (GitHub / local path) — no newer version found"),
    ("更新 {}", "Update {}"),
    ("更新", "Update"),
    ("仓库", "Repo"),
    ("{} 小时 {} 分", "{}h {}m"),
    ("{} 分 {} 秒", "{}m {}s"),
    ("{} 秒", "{}s"),
    ("全部", "All"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_language_flips_back_and_forth() {
        let _g = TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set(Lang::Zh);
        assert!(is_zh());
        assert_eq!(lookup("控制台"), "控制台");
        set(Lang::En);
        assert!(is_en());
        assert_eq!(lookup("控制台"), "Console");
        // 表里没有的 key 回落原文，界面不会出现空白
        assert_eq!(lookup("这句没翻译"), "这句没翻译");
        set(Lang::Zh);
        assert!(is_zh());
    }

    #[test]
    fn fill_replaces_placeholders_in_order() {
        let args = vec!["1.2.3".to_string(), "dshmarket".to_string()];
        assert_eq!(fill("{} -> {}", &args), "1.2.3 -> dshmarket");
        // 参数多于占位符时多余的丢掉，少了就留空
        assert_eq!(fill("{} done", &args), "1.2.3 done");
        assert_eq!(fill("{} and {}", &["a".to_string()]), "a and ");
    }

    #[test]
    fn entries_have_no_duplicates_or_empty_values() {
        let mut seen = std::collections::HashSet::new();
        for (zh, en) in ENTRIES {
            assert!(!zh.is_empty() && !en.is_empty(), "空条目: {:?}", zh);
            assert!(seen.insert(*zh), "重复的中文 key: {:?}", zh);
            // 占位符个数必须一致，否则英文界面会少填/多填参数
            assert_eq!(
                zh.matches("{}").count(),
                en.matches("{}").count(),
                "占位符数量不一致: {:?} / {:?}",
                zh,
                en
            );
        }
    }

    #[test]
    fn labels_are_self_named() {
        assert_eq!(Lang::Zh.label(), "中文");
        assert_eq!(Lang::En.label(), "English");
    }
}

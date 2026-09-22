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
    ("已清除补丁条目 {}", "Removed patch entry {}"),
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
    ("启动 / 关闭", "Start / Stop"),
    ("版本", "Versions"),
    ("安装与切换", "Install & switch"),
    ("插件", "Plugins"),
    ("搜索与管理", "Search & manage"),
    ("设置", "Settings"),
    ("服务与行为", "Service & behavior"),
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
    ("补丁引用", "patch only"),
    ("profile package.json 的 dependencies 里登记着（dsh plugin add / pnpm add）", "Declared in the profile package.json dependencies (dsh plugin add / pnpm add)"),
    ("在 profile 的 dsh.profile.bundles 里，会被加载进 boot graph", "Listed in the profile's dsh.profile.bundles; loaded into the boot graph"),
    ("文件实际存在于 profile 的 node_modules（可能没写进 package.json）", "Files exist in the profile's node_modules (may not be listed in package.json)"),
    ("位于 pnpm 虚拟存储 .pnpm 里", "Lives in the pnpm virtual store (.pnpm)"),
    ("DSH 的模块兜底目录 .dsh-module-fallback/node_modules", "DSH's module fallback dir .dsh-module-fallback/node_modules"),
    ("<DSH_HOME>/profiles/node_modules 共享目录（多个 profile 共用）", "<DSH_HOME>/profiles/node_modules, shared by every profile"),
    ("由全局 dsh 安装自带、随 profile 选择加载，不是用户装的（插件页不列出这些平台层）", "Ships with the global dsh install and loads with the profile; not user-installed (these platform layers are not listed here)"),
    ("只在 profile 的 cordis.patch.yml 里被引用，没找到对应包", "Referenced only in the profile's cordis.patch.yml; no matching package found"),
    ("补丁条目 id: {}", "Patch entry id: {}"),
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
    ("补丁里没有 id 为 {} 的条目", "No patch entry with id {}"),
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
    ("补丁条目（{}）", "Patch entries ({})"),
    ("这些 id 出现在 profile 的 cordis.patch.yml 里，但没找到对应的插件包（可能是官方模块的启停行，或插件已被删掉）。", "These ids appear in the profile's cordis.patch.yml but no matching plugin package was found (they may be enable/disable rows for official modules, or a plugin that was removed)."),
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
    ("从 cordis.patch.yml 里清除补丁条目 {}？（会先备份成 .bak）", "Remove patch entry {} from cordis.patch.yml? (a .bak backup is kept first)"),
    ("确认更新", "Confirm update"),
    ("清除", "Remove"),
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

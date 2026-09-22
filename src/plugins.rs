//! DSH 插件管理：搜索、安装、启用/禁用。
//!
//! 数据源（按可靠性排序）：
//! 1. `awesome-dsh-plugin.com/plugins.json` —— 社区聚合源，4000+ 条，
//!    含 npm 包名 / 分类 / star 数 / 双语描述 / 现成安装命令
//! 2. GitHub 搜索（`topic:dsh-plugin`）—— 兜底，覆盖聚合源尚未收录的仓库
//!
//! 启停机制：DSH profile 的 `cordis.patch.yml` 里
//! `- id: <插件 id>` + `disabled: true/false`。
//! **改写时按行插入、保留注释**（该文件通常带用户手写注释，不能整体 YAML 重排）。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::procs;

// ------------------------------------------------------------------ 数据结构

/// 插件市场条目。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginInfo {
    pub name: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub description_zh: String,
    /// npm 包名（为空表示只能从 GitHub 装）
    #[serde(default)]
    pub npm: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub stars: u64,
    #[serde(default)]
    pub downloads: u64,
    /// 现成的安装参数（`dsh plugin --profile web add <这部分>`）
    #[serde(default)]
    pub install: String,
}

/// 一个包是**从哪条途径**被发现的。
///
/// DSH 的插件层没有唯一入口：它可能在 profile 的 `package.json` 里登记，
/// 也可能只是被选进 `dsh.profile.bundles`，还可能由 pnpm / 手工 / 其它工具
/// 直接落在某个 `node_modules` 里。把途径记下来，界面上才解释得清
/// "这个插件是怎么进来的、为什么能/不能启停"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PluginRoute {
    /// profile `package.json` 的 dependencies（`dsh plugin add` / pnpm add 装的）
    Dependency,
    /// profile `package.json` 的 `dsh.profile.bundles`（被选中的加载层）
    BundleLayer,
    /// 实际躺在 profile 自己的 `node_modules` 里
    ProfileModules,
    /// pnpm 虚拟存储 `node_modules/.pnpm/<name>@<ver>/node_modules`
    PnpmStore,
    /// `.dsh-module-fallback/node_modules`（DSH 的模块兜底目录）
    FallbackModules,
    /// `<DSH_HOME>/profiles/node_modules` 这类上层共享目录
    SharedModules,
    /// 全局 dsh 安装自带（平台层，随 profile 选择加载）
    Installation,
    /// 只在 profile 的 `cordis.patch.yml` 里被按 id 引用
    PatchOnly,
}

impl PluginRoute {
    /// 界面上的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Dependency => "依赖",
            Self::BundleLayer => "bundle 层",
            Self::ProfileModules => "node_modules",
            Self::PnpmStore => "pnpm 存储",
            Self::FallbackModules => "兜底目录",
            Self::SharedModules => "共享目录",
            Self::Installation => "dsh 自带",
            Self::PatchOnly => "补丁引用",
        }
    }

    /// 悬停时的一句话解释。
    pub fn hint(self) -> &'static str {
        match self {
            Self::Dependency => "profile package.json 的 dependencies 里登记着（dsh plugin add / pnpm add）",
            Self::BundleLayer => "在 profile 的 dsh.profile.bundles 里，会被加载进 boot graph",
            Self::ProfileModules => "文件实际存在于 profile 的 node_modules（可能没写进 package.json）",
            Self::PnpmStore => "位于 pnpm 虚拟存储 .pnpm 里",
            Self::FallbackModules => "DSH 的模块兜底目录 .dsh-module-fallback/node_modules",
            Self::SharedModules => "<DSH_HOME>/profiles/node_modules 共享目录（多个 profile 共用）",
            Self::Installation => "由全局 dsh 安装自带、随 profile 选择加载，不是用户装的（插件页不列出这些平台层）",
            Self::PatchOnly => "只在 profile 的 cordis.patch.yml 里被引用，没找到对应包",
        }
    }

    /// 汇总行里的固定顺序。
    pub fn all() -> [Self; 8] {
        [
            Self::Dependency,
            Self::BundleLayer,
            Self::ProfileModules,
            Self::PnpmStore,
            Self::FallbackModules,
            Self::SharedModules,
            Self::Installation,
            Self::PatchOnly,
        ]
    }
}

/// 这个包算"用户装的"还是"DSH 自带的"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginOrigin {
    /// 用户（或 profile）自己装进来的
    #[default]
    Profile,
    /// 全局 dsh 安装的依赖，用户没装过——只是随 profile 的选择被加载
    Installation,
}

/// 已安装插件（从 profile 的多条安装途径归并推导）。
#[derive(Debug, Clone, Default)]
pub struct InstalledPlugin {
    /// 包名（`patch_only` 为真时这里是补丁里的 id）
    pub package: String,
    /// 可由 profile 补丁启停的 cordis 条目 id（可能多个，如一个包 insert 了多行）
    pub ids: Vec<String>,
    /// 已安装版本（解析到包目录时用真实版本，否则回落到声明里的版本范围）
    pub version: String,
    pub enabled: bool,
    /// 是否声明了插件层补丁（`dsh.bundle.patch`，或随包带 `cordis.patch.yml`）
    pub layer: bool,
    pub origin: PluginOrigin,
    /// 命中的安装途径（去重、按固定顺序）
    pub routes: Vec<PluginRoute>,
    /// 解析到的包目录
    pub dir: Option<PathBuf>,
    /// 只是补丁里的一个 id，没有对应的包（不能卸载）
    pub patch_only: bool,
}

impl InstalledPlugin {
    /// 悬停详情：每条途径一行解释。
    pub fn route_detail(&self) -> String {
        self.routes
            .iter()
            .map(|r| format!("· {}：{}", r.label(), r.hint()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 启停状态与 id 的说明行。
    pub fn id_line(&self) -> String {
        if self.patch_only {
            return format!("补丁条目 id: {}", self.package);
        }
        if !self.ids.is_empty() {
            return format!("插件 id: {}", self.ids.join(", "));
        }
        if self.dir.is_none() {
            return "磁盘上没找到这个包（可能已被删除，或没装到这个 profile）".to_string();
        }
        if self.origin == PluginOrigin::Installation {
            return "平台层：随 profile 的 bundles 加载，不在这里单独启停".to_string();
        }
        if self.layer {
            return "该包只做 id 覆盖、不 insert 自己的条目，没有可启停的插件".to_string();
        }
        "未声明插件层（dsh.bundle.patch），无法启停".to_string()
    }
}

// ------------------------------------------------------------------ 市场源

const REGISTRY_URL: &str = "https://awesome-dsh-plugin.com/plugins.json";

/// 抓取插件聚合源（阻塞）。
pub fn fetch_market() -> Result<Vec<PluginInfo>, String> {
    let body: serde_json::Value = config::http_agent()
        .get(REGISTRY_URL)
        .call()
        .map_err(|e| format!("无法访问插件市场: {}", e))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("插件市场返回内容无法解析: {}", e))?;

    let arr = body
        .get("plugins")
        .and_then(|x| x.as_array())
        .ok_or_else(|| "插件市场返回结构异常（缺少 plugins 数组）".to_string())?;

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let desc_zh = item
            .get("description")
            .and_then(|d| d.get("zh"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let desc_en = item
            .get("description")
            .and_then(|d| d.get("en"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let install_full = item.get("install").and_then(|x| x.as_str()).unwrap_or("");
        out.push(PluginInfo {
            name: item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            owner: item.get("owner").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            url: item.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            category: item.get("category").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            description: desc_en,
            description_zh: desc_zh,
            npm: item.get("npm").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            version: item.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            stars: item.get("stars").and_then(|x| x.as_u64()).unwrap_or(0),
            downloads: item.get("downloads").and_then(|x| x.as_u64()).unwrap_or(0),
            install: install_spec(install_full),
        });
    }
    Ok(out)
}

/// 从聚合源的整条命令里剥离前缀，只留包规格。
/// `dsh plugin --profile web add dsh-pet` → `dsh-pet`
fn install_spec(full: &str) -> String {
    let Some(pos) = full.find(" add ") else {
        return full.trim().to_string();
    };
    full[pos + 5..].trim().trim_matches('"').to_string()
}

/// GitHub 搜索兜底（`topic:dsh-plugin` + 关键词）。
pub fn search_github(query: &str) -> Result<Vec<PluginInfo>, String> {
    let q = if query.trim().is_empty() {
        "topic:dsh-plugin".to_string()
    } else {
        format!("{} topic:dsh-plugin", query.trim())
    };
    let url = format!(
        "https://api.github.com/search/repositories?q={}&sort=stars&order=desc&per_page=40",
        urlencode(&q)
    );
    let body: serde_json::Value = config::http_agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("GitHub 搜索失败: {}", e))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("GitHub 返回内容无法解析: {}", e))?;

    let mut out = Vec::new();
    for item in body.get("items").and_then(|x| x.as_array()).into_iter().flatten() {
        let full_name = item.get("full_name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let owner = full_name.split('/').next().unwrap_or("").to_string();
        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
        out.push(PluginInfo {
            name: name.clone(),
            owner,
            url: item.get("html_url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            category: "github".to_string(),
            description: item.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            description_zh: String::new(),
            npm: String::new(),
            version: String::new(),
            stars: item.get("stargazers_count").and_then(|x| x.as_u64()).unwrap_or(0),
            downloads: 0,
            // 没有 npm 包名时按 GitHub 仓库装（pnpm 支持 github:owner/repo）
            install: format!("github:{}", full_name),
        });
    }
    Ok(out)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 分类的中文名。
///
/// 市场数据里的分类键是英文（ui / theme / fun…），**只在显示时翻译**：
/// 过滤仍然按英文键比较，所以市场新增分类也不会因为没翻译就筛不出来。
/// 未知分类原样返回，避免界面上出现空白标签。
pub fn category_label(key: &str) -> String {
    let zh = match key {
        "ui" => "界面",
        "theme" => "主题",
        "fun" => "趣味",
        "tools" => "工具",
        "model" => "模型",
        "usage" => "用量",
        "session" => "会话",
        "memory" => "记忆",
        "notify" => "通知",
        "workflow" => "工作流",
        "git" => "Git",
        "docs" => "文档",
        "voice" => "语音",
        "vision" => "视觉",
        "security" => "安全",
        "market" => "市场",
        // GitHub 搜索结果自带这个分类，专有名词保持原样
        "github" => "GitHub",
        _ => return key.to_string(),
    };
    zh.to_string()
}

/// 在本地列表里做关键词过滤（分类 + 名称 + 描述 + owner）。
pub fn filter_local(list: &[PluginInfo], query: &str, category: &str) -> Vec<PluginInfo> {
    let q = query.trim().to_lowercase();
    list.iter()
        .filter(|p| {
            if !category.is_empty() && category != "全部" && p.category != category {
                return false;
            }
            if q.is_empty() {
                return true;
            }
            p.name.to_lowercase().contains(&q)
                || p.owner.to_lowercase().contains(&q)
                || p.description.to_lowercase().contains(&q)
                || p.description_zh.to_lowercase().contains(&q)
                || p.npm.to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

// ------------------------------------------------------------------ profile 路径

/// 某个 profile 的目录。
pub fn profile_dir(profile: &str) -> PathBuf {
    config::dsh_home().join("profiles").join(profile)
}

/// profile 的 `cordis.patch.yml`。
pub fn patch_file(profile: &str) -> PathBuf {
    profile_dir(profile).join("cordis.patch.yml")
}

/// profile 的 `package.json`。
pub fn profile_package_json(profile: &str) -> PathBuf {
    profile_dir(profile).join("package.json")
}

// ------------------------------------------------------------------ 已装插件

// ------------------------------------------------------------------ 多途径扫描

/// 扫描插件时的定位上下文。
///
/// 抽出来是为了能在测试里指向一棵 fixture 目录树，不去碰真实的 `~/.dsh`。
#[derive(Debug, Clone)]
pub struct ScanContext {
    /// DSH_HOME（`profiles/` 在它下面）
    pub home: PathBuf,
    /// 全局 dsh 包目录（`…/@deepseek-ai/dsh`），用来区分"安装自带"的层
    pub install: Option<PathBuf>,
}

impl ScanContext {
    /// 从运行环境探测。
    pub fn detect() -> Self {
        Self { home: config::dsh_home(), install: crate::dsh::dsh_package_dir() }
    }

    /// 某个 profile 的目录。
    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.home.join("profiles").join(profile)
    }

    /// profile 的 `cordis.patch.yml`。
    pub fn patch_file(&self, profile: &str) -> PathBuf {
        self.profile_dir(profile).join("cordis.patch.yml")
    }
}

/// 一次扫描的完整结果。
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub profile: String,
    /// 用户装的插件（按包名排序）
    pub plugins: Vec<InstalledPlugin>,
    /// 全局 dsh 安装自带、且被 profile 选中的层（dsh-base / dsh-web-app…）
    pub builtin: Vec<InstalledPlugin>,
    /// profile 补丁里出现、但找不到对应包的 id
    pub patch_rows: Vec<InstalledPlugin>,
    /// 每条途径命中了几个包
    pub counts: Vec<(PluginRoute, usize)>,
    /// 扫描过程中的提示（界面折叠显示）
    pub warnings: Vec<String>,
    /// 扫描时刻（界面显示"刚刚 / N 秒前"）
    pub scanned_at: Option<std::time::Instant>,
    /// 实际查过的目录（诊断用）
    pub roots: Vec<String>,
}

/// 扫描某个 profile 的插件（阻塞，读的只是本地文件，很快）。
pub fn scan(profile: &str) -> ScanReport {
    scan_with(&ScanContext::detect(), profile)
}

/// 核心：把 profile 里**所有**能发现插件的途径合并成一份列表。
///
/// 途径（与 DSH 自己的 `pluginManager.listBundles()` 对齐，并额外覆盖
/// "没写进 package.json、但确实躺在磁盘上"的漏网插件）：
/// 1. profile `package.json` 的 dependencies / `dsh.profile.bundles`
/// 2. profile 自己的 `node_modules`（含 `.pnpm` 虚拟存储、`.dsh-module-fallback`）
/// 3. `<DSH_HOME>/profiles/node_modules` 等上层共享目录（解析用）
/// 4. 全局 dsh 安装自带的依赖（平台层）
/// 5. profile `cordis.patch.yml` 里按 id 引用的条目
///
/// 只有声明了插件层补丁（`dsh.bundle.patch`，或随包带 `cordis.patch.yml`）
/// 的包才会被物理扫描收进来——否则 `js-yaml` / `undici` 这些传递依赖
/// 会被误当成插件。
pub fn scan_with(ctx: &ScanContext, profile: &str) -> ScanReport {
    let mut rep = ScanReport {
        profile: profile.to_string(),
        scanned_at: Some(std::time::Instant::now()),
        ..Default::default()
    };
    let dir = ctx.profile_dir(profile);
    rep.roots = resolution_roots(ctx, profile)
        .into_iter()
        .map(|(p, _)| p.display().to_string())
        .collect();

    let doc = read_json(&dir.join("package.json"));
    if doc.is_none() && !dir.is_dir() {
        rep.warnings.push(format!("profile 目录不存在：{}", dir.display()));
    } else if doc.is_none() {
        rep.warnings
            .push(format!("读不到或解析不了 {}", profile_package_json(profile).display()));
    }
    let deps = dep_map(doc.as_ref());
    let bundles = bundle_list(doc.as_ref());
    let install_deps = match &ctx.install {
        Some(d) => dep_map(read_json(&d.join("package.json")).as_ref()),
        None => {
            rep.warnings
                .push("未解析到全局 dsh 安装，无法区分平台自带层".to_string());
            Default::default()
        }
    };

    let profile_patch = std::fs::read_to_string(ctx.patch_file(profile)).unwrap_or_default();

    // ---- 1) 归并候选：包名 → 命中的途径 + 声明里的版本
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    for (name, ver) in &deps {
        let f = found.entry(name.clone()).or_default();
        f.routes.push(PluginRoute::Dependency);
        if f.version.is_empty() {
            f.version = ver.clone();
        }
    }
    for name in &bundles {
        let f = found.entry(name.clone()).or_default();
        f.routes.push(PluginRoute::BundleLayer);
        f.bundled = true;
    }
    for name in install_deps.keys() {
        if deps.contains_key(name) {
            continue; // 用户自己装过这一份 → 算用户插件，不算"自带"
        }
        found.entry(name.clone()).or_default().routes.push(PluginRoute::Installation);
    }
    // ---- 2) 物理扫描：profile 自己的 node_modules / .pnpm / 兜底目录
    let physical = physical_packages(ctx, profile);
    for (name, (_, route)) in &physical {
        found.entry(name.clone()).or_default().routes.push(*route);
    }

    // ---- 3) 逐个解析目录、补丁、id
    let mut builtin: Vec<InstalledPlugin> = Vec::new();
    let mut plugins: Vec<InstalledPlugin> = Vec::new();
    let mut claimed_ids: BTreeSet<String> = BTreeSet::new();
    for (name, mut f) in found {
        let resolved = physical
            .get(&name)
            .map(|(d, r)| (d.clone(), *r))
            .or_else(|| resolve_package(ctx, profile, &name));
        let (pkg_doc, pkg_dir) = match &resolved {
            Some((d, route)) => {
                f.routes.push(*route);
                (read_json(&d.join("package.json")), Some(d.clone()))
            }
            None => (None, None),
        };
        let layer_patch = pkg_dir.as_deref().and_then(layer_patch);
        let origin = if install_deps.contains_key(&name) && !deps.contains_key(&name) {
            PluginOrigin::Installation
        } else {
            PluginOrigin::Profile
        };
        // 平台层**不给启停入口**：dsh-base / dsh-web-app 这类包的 insert 块是整个底座
        // （实测 dsh-base 的补丁里 insert 了上百个模块），逐行禁用等于把 DSH 拆了。
        // 平台层的"开关"是 profile 的 dsh.profile.bundles，不在这里。
        let ids = match (&pkg_dir, &layer_patch) {
            _ if origin == PluginOrigin::Installation => Vec::new(),
            (Some(d), Some(p)) => plugin_ids(d, p, &name),
            _ => Vec::new(),
        };
        if origin == PluginOrigin::Installation && !f.bundled {
            // 安装自带、又没被这个 profile 选中：与当前 profile 无关，不列
            continue;
        }
        let version = pkg_doc
            .as_ref()
            .and_then(|d| d.get("version"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or(f.version.clone());
        let enabled = ids.is_empty()
            || !ids.iter().any(|i| is_disabled_in_patch(&profile_patch, i));
        for i in &ids {
            claimed_ids.insert(i.clone());
        }
        let item = InstalledPlugin {
            package: name.clone(),
            ids,
            version,
            enabled,
            layer: layer_patch.is_some(),
            origin,
            routes: dedup_routes(f.routes),
            dir: pkg_dir,
            patch_only: false,
        };
        if origin == PluginOrigin::Installation {
            builtin.push(item);
        } else {
            plugins.push(item);
        }
    }

    // ---- 4) profile 补丁里没被任何包认领的 id
    let mut patch_rows: Vec<InstalledPlugin> = Vec::new();
    for id in top_level_patch_ids(&profile_patch) {
        if claimed_ids.contains(&id) {
            continue;
        }
        let enabled = !is_disabled_in_patch(&profile_patch, &id);
        patch_rows.push(InstalledPlugin {
            package: id.clone(),
            ids: vec![id],
            version: String::new(),
            enabled,
            layer: false,
            origin: PluginOrigin::Profile,
            routes: vec![PluginRoute::PatchOnly],
            dir: None,
            patch_only: true,
        });
    }

    plugins.sort_by(|a, b| a.package.cmp(&b.package));
    builtin.sort_by(|a, b| a.package.cmp(&b.package));
    patch_rows.sort_by(|a, b| a.package.cmp(&b.package));

    let mut counts: BTreeMap<PluginRoute, usize> = BTreeMap::new();
    for p in plugins.iter().chain(builtin.iter()).chain(patch_rows.iter()) {
        for r in &p.routes {
            *counts.entry(*r).or_insert(0) += 1;
        }
    }
    rep.counts = PluginRoute::all().iter().map(|r| (*r, counts.get(r).copied().unwrap_or(0))).collect();
    rep.plugins = plugins;
    rep.builtin = builtin;
    rep.patch_rows = patch_rows;
    rep
}

/// 候选项在归并过程中的临时状态。
#[derive(Debug, Default)]
struct Found {
    routes: Vec<PluginRoute>,
    version: String,
    bundled: bool,
}

// ------------------------------------------------------------------ 文件与清单助手

/// 读一个 JSON 文件；任何失败都返回 None（插件页不该被一个坏文件拖垮）。
fn read_json(path: &Path) -> Option<serde_json::Value> {
    let txt = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&txt).ok()
}

/// manifest 里的 dependencies（包名 → 版本声明）。
fn dep_map(doc: Option<&serde_json::Value>) -> BTreeMap<String, String> {
    doc.and_then(|d| d.get("dependencies"))
        .and_then(|d| d.as_object())
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// manifest 里的 `dsh.profile.bundles`。
fn bundle_list(doc: Option<&serde_json::Value>) -> Vec<String> {
    doc.and_then(|d| d.get("dsh"))
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// 去掉 YAML 标量两侧的引号与行尾注释。
fn clean_scalar(s: &str) -> String {
    let s = s.trim();
    let s = match s.find(" #") {
        Some(i) => &s[..i],
        None => s,
    };
    s.trim().trim_matches('"').trim_matches('\'').trim().to_string()
}

/// 一个包声明的插件层补丁（相对包目录的路径）。
///
/// 权威来源是 package.json 的 `dsh.bundle.patch`；没写这个字段、但随包带了
/// `cordis.patch.yml` 的也认。
fn layer_patch(dir: &Path) -> Option<String> {
    if let Some(doc) = read_json(&dir.join("package.json")) {
        if let Some(p) = doc
            .get("dsh")
            .and_then(|d| d.get("bundle"))
            .and_then(|b| b.get("patch"))
            .and_then(|x| x.as_str())
        {
            return Some(p.to_string());
        }
    }
    dir.join("cordis.patch.yml")
        .is_file()
        .then(|| "cordis.patch.yml".to_string())
}

/// 模块名对不上包名时，最多认这么多条 insert 行（区分"插件自己的层"与"官方底座"）。
const MAX_FALLBACK_INSERTS: usize = 5;

/// 一个包**自己 insert** 进 profile 的 cordis 条目 id。
///
/// 只看 `- insert:` 列表里、模块名指向本包的条目。实测三种真实形态：
/// `dsh-bloom-theme` → `bloom-theme`（name 就是包名）、`dshmarket` → `dsh-market`、
/// `@michengai/dsh-archive-manager` → 三个 id（name 是 `包名`、`包名/workspace`、
/// `包名/projcache`）。
///
/// **绝不看顶层 `- id:`**：`@deepseek-ai/dsh-web-app` 这类 bundle 的补丁里有几十行
/// 覆盖官方子系统的 `- id: system-prompt` / `- id: tools`，把第一行当成"这个插件的 id"
/// 会在启停时关掉系统组件（老实现就是栽在这里）。没有 insert 行的包=只提供配置覆盖，
/// 界面不给启停入口。
fn plugin_ids(dir: &Path, patch_rel: &str, package: &str) -> Vec<String> {
    let Ok(txt) = std::fs::read_to_string(dir.join(patch_rel)) else {
        return Vec::new();
    };
    let mut insert: Vec<(String, String)> = Vec::new(); // (id, name)
    let mut in_insert = false;
    let mut insert_indent = 0usize;
    let mut cur_id: Option<String> = None;
    for line in txt.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if t.starts_with("- insert:") {
            in_insert = true;
            insert_indent = indent;
            cur_id = None;
            continue;
        }
        if in_insert && indent <= insert_indent {
            in_insert = false; // insert 块结束，回到顶层
        }
        if !in_insert {
            continue;
        }
        if let Some(rest) = t.strip_prefix("- id:").or_else(|| t.strip_prefix("id:")) {
            cur_id = Some(clean_scalar(rest));
        } else if let Some(rest) = t.strip_prefix("name:") {
            if let Some(id) = cur_id.take() {
                insert.push((id, clean_scalar(rest)));
            }
        }
    }
    let owned = format!("{}/", package);
    let mut ids: Vec<String> = insert
        .iter()
        .filter(|(_, n)| n == package || n.starts_with(&owned))
        .map(|(id, _)| id.clone())
        .collect();
    if ids.is_empty() && !insert.is_empty() && insert.len() <= MAX_FALLBACK_INSERTS {
        // 模块名写成相对路径（`./lib/x.js`）等形态：这个包的 insert 行都算它的层。
        // **只在 insert 很少时才认**：官方底座那种上百行的 insert 一旦被当成
        // "这个包的 id 列表"，一次误点就会关掉整个 DSH。
        ids = insert.into_iter().map(|(id, _)| id).collect();
    }
    ids.sort();
    ids.dedup();
    ids
}

/// profile 补丁里**顶层**的条目 id（insert 块里的不算）。
fn top_level_patch_ids(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut base: Option<usize> = None;
    let mut in_insert = false;
    let mut insert_indent = 0usize;
    for line in patch.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if t.starts_with("- insert:") {
            in_insert = true;
            insert_indent = indent;
            continue;
        }
        if in_insert {
            if indent <= insert_indent {
                in_insert = false;
            } else {
                continue;
            }
        }
        if !t.starts_with("- ") {
            continue;
        }
        let b = *base.get_or_insert(indent);
        if indent != b {
            continue; // 子行（config: 之类）
        }
        if let Some(rest) = t.strip_prefix("- id:") {
            out.push(clean_scalar(rest));
        }
    }
    out.sort();
    out.dedup();
    out
}

// ------------------------------------------------------------------ 物理扫描与解析

/// 列出一个 node_modules 里的包名（含 `@scope/name`）。
///
/// 用 `is_dir()` 判定，所以 pnpm 的符号链接 / junction（`profiles/node_modules`
/// 里的 `@deepseek-ai/*` 就是 junction）都会被跟随；断链自然跳过。
fn scan_packages(root: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(scope) = name.strip_prefix('@') {
            if let Ok(sub) = std::fs::read_dir(&path) {
                for s in sub.flatten() {
                    if s.path().join("package.json").is_file() {
                        out.push(format!("@{}/{}", scope, s.file_name().to_string_lossy()));
                    }
                }
            }
        } else if path.join("package.json").is_file() {
            out.push(name);
        }
    }
    out.sort();
    out
}

/// 列出 pnpm 虚拟存储 `.pnpm/<name>@<ver>/node_modules/` 里的包。
fn scan_pnpm_store(node_modules: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(node_modules.join(".pnpm")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let inner = e.path().join("node_modules");
        for name in scan_packages(&inner) {
            out.push((name.clone(), inner.join(&name)));
        }
    }
    out
}

/// profile 自己地盘里的包（物理存在即算），只收声明了插件层的。
///
/// 只看 profile 自己的目录：`<DSH_HOME>/profiles/node_modules` 那种共享目录里有
/// 上百个平台包，整目录扫会把界面淹掉；共享目录只参与"解析"。
fn physical_packages(ctx: &ScanContext, profile: &str) -> BTreeMap<String, (PathBuf, PluginRoute)> {
    let dir = ctx.profile_dir(profile);
    let mut out: BTreeMap<String, (PathBuf, PluginRoute)> = BTreeMap::new();

    let nm = dir.join("node_modules");
    for name in scan_packages(&nm) {
        let path = nm.join(&name);
        if layer_patch(&path).is_some() {
            out.insert(name, (path, PluginRoute::ProfileModules));
        }
    }
    for (name, path) in scan_pnpm_store(&nm) {
        if layer_patch(&path).is_some() {
            out.entry(name).or_insert((path, PluginRoute::PnpmStore));
        }
    }
    let fb = dir.join(".dsh-module-fallback").join("node_modules");
    for name in scan_packages(&fb) {
        let path = fb.join(&name);
        if layer_patch(&path).is_some() {
            out.entry(name).or_insert((path, PluginRoute::FallbackModules));
        }
    }
    out
}

/// 一个包名的解析顺序（Node 的 node_modules 上溯顺序的简化版），带层次标签。
fn resolution_roots(ctx: &ScanContext, profile: &str) -> Vec<(PathBuf, PluginRoute)> {
    let dir = ctx.profile_dir(profile);
    let mut out = vec![
        (dir.join("node_modules"), PluginRoute::ProfileModules),
        (
            dir.join(".dsh-module-fallback").join("node_modules"),
            PluginRoute::FallbackModules,
        ),
        (
            ctx.home.join("profiles").join("node_modules"),
            PluginRoute::SharedModules,
        ),
        (ctx.home.join("node_modules"), PluginRoute::SharedModules),
    ];
    if let Some(install) = &ctx.install {
        out.push((install.join("node_modules"), PluginRoute::Installation));
        // …/npm/node_modules/@deepseek-ai/dsh → …/npm/node_modules
        if let Some(nm) = install.parent().and_then(|p| p.parent()) {
            out.push((nm.to_path_buf(), PluginRoute::Installation));
        }
    }
    out
}

/// 定位一个包，并说明是在哪一层找到的。
fn resolve_package(ctx: &ScanContext, profile: &str, name: &str) -> Option<(PathBuf, PluginRoute)> {
    resolution_roots(ctx, profile)
        .into_iter()
        .find_map(|(root, route)| {
            let cand = root.join(name);
            cand.join("package.json").is_file().then_some((cand, route))
        })
}

/// 途径去重 + 按固定顺序排列。
fn dedup_routes(mut routes: Vec<PluginRoute>) -> Vec<PluginRoute> {
    routes.sort();
    routes.dedup();
    routes
}

// ------------------------------------------------------------------ 启停（改写 patch）

/// 在 patch 文本里判断某 id 是否被禁用。
///
/// 逐个 `- id: <id>` 条目块扫描，块内出现 `disabled: true` 即禁用。
pub fn is_disabled_in_patch(patch: &str, want_id: &str) -> bool {
    let mut in_block = false;
    for line in patch.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- id:") {
            in_block = rest.trim().trim_matches('"').trim_matches('\'') == want_id;
            continue;
        }
        // 新条目开始（`- insert:` 等）→ 当前块结束
        if t.starts_with("- ") {
            in_block = false;
            continue;
        }
        if in_block && t.starts_with("disabled:") {
            return t.contains("true");
        }
    }
    false
}

/// 在 patch 文本里找到某 id 的条目块行范围：`(起始行, 结束行exclusive)`。
fn block_range(lines: &[&str], want_id: &str) -> Option<(usize, usize)> {
    let mut start = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- id:") {
            if rest.trim().trim_matches('"').trim_matches('\'') == want_id {
                start = Some(i);
                continue;
            }
            if start.is_some() {
                return Some((start.unwrap(), i));
            }
        } else if t.starts_with("- ") && start.is_some() {
            return Some((start.unwrap(), i));
        }
    }
    start.map(|s| (s, lines.len()))
}

/// 顶层文档是不是「空的流式数组」——DSH 生成的默认 `cordis.patch.yml` 就是
/// 这个形态（几行注释 + 一行 `[]`）。
fn is_empty_flow_seq(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 2 && t.starts_with('[') && t.ends_with(']') && t[1..t.len() - 1].trim().is_empty()
}

/// 顶层「文档行」：最后一行既不是空行也不是注释。
fn document_line(lines: &[String]) -> Option<usize> {
    lines.iter().rposition(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    })
}

/// 设置某插件的启用状态；返回改写后的文本。
///
/// 已存在条目 → 原地改 `disabled:`；不存在 → 追加一个顶层条目。
/// **保留原有注释与缩进风格**（不做整体 YAML 重排）。
///
/// 追加时若顶层是空的流式数组 `[]`（DSH 自带默认内容），必须先把它去掉——
/// `[]` 后面再跟块序列是**非法 YAML**，会让 DSH 下次启动时读不了 profile。
/// 非空的流式数组（`[{…}]`）形态不做自动改写，直接报错让用户手工处理，
/// 宁可不写也不写坏。
pub fn set_enabled_in_patch(patch: &str, id: &str, enabled: bool) -> Result<String, String> {
    let mut lines: Vec<String> = patch.lines().map(|s| s.to_string()).collect();
    let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();

    if let Some((start, end)) = block_range(&refs, id) {
        let indent = lines[start].len() - lines[start].trim_start().len();
        let child_indent = " ".repeat(indent + 2);
        let mut found = false;
        for line in lines.iter_mut().take(end).skip(start + 1) {
            let t = line.trim_start();
            if t.starts_with("disabled:") {
                *line = format!("{}disabled: {}", child_indent, !enabled);
                found = true;
                break;
            }
        }
        if !found {
            // 该条目没写 disabled：在 `- id:` 之后插入一行
            lines.insert(start + 1, format!("{}disabled: {}", child_indent, !enabled));
        }
    } else {
        // 新增顶层条目：先处理「空的流式数组根」
        if let Some(i) = document_line(&lines) {
            let root = lines[i].trim().to_string();
            if is_empty_flow_seq(&root) {
                lines.remove(i);
            } else if root.starts_with('[') {
                return Err(
                    "该文件用的是流式数组写法（如 [{...}]），启动器不会自动改写以免破坏配置；\
                     请先把它改成块序列（每行 `- id: ...`）再试。"
                        .to_string(),
                );
            }
        }
        if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        lines.push(format!("- id: {}", id));
        lines.push(format!("  disabled: {}", !enabled));
    }

    let mut out = lines.join("\n");
    if patch.ends_with('\n') || !patch.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

// ------------------------------------------------------------------ 安装 / 卸载

/// `dsh plugin` 是**转发给 pnpm** 的（见 dsh 的 plugin 实现），
/// pnpm 不在 PATH 上时先给出能照做的提示，而不是把 pnpm 的报错原样丢给用户。
fn preflight_pnpm() -> Result<(), String> {
    if crate::dsh::which("pnpm").is_some() {
        return Ok(());
    }
    Err("未找到 pnpm。dsh 的插件命令是转发给 pnpm 执行的，请先安装：\n  npm install -g pnpm\n（或 corepack enable）".to_string())
}

/// 用 DSH 自带的插件命令安装（走 pnpm，与 dsh-market 相同的路径）。
///
/// 执行的是**全局安装**那一份 dsh——与启动服务、与服务端加载插件的是同一份。
pub fn install(profile: &str, spec: &str) -> Result<String, String> {
    preflight_pnpm()?;
    let (program, prefix) = crate::dsh::cli_runner()?;
    let mut argv: Vec<String> = prefix;
    argv.extend(
        ["plugin", "--profile", profile, "add", spec].iter().map(|s| s.to_string()),
    );
    let args: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let (ok, text) = procs::run_capture(&program, &args, None)?;
    if ok {
        Ok(text)
    } else {
        Err(format!("安装失败：\n{}\n\n可手动执行：\ndsh plugin --profile {} add {}", text, profile, spec))
    }
}

/// 卸载插件。
pub fn uninstall(profile: &str, spec: &str) -> Result<String, String> {
    preflight_pnpm()?;
    let (program, prefix) = crate::dsh::cli_runner()?;
    let mut argv: Vec<String> = prefix;
    argv.extend(
        ["plugin", "--profile", profile, "remove", spec].iter().map(|s| s.to_string()),
    );
    let args: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let (ok, text) = procs::run_capture(&program, &args, None)?;
    if ok {
        Ok(text)
    } else {
        Err(format!("卸载失败：\n{}", text))
    }
}

/// 把「启用/禁用」写回 profile 的 cordis.patch.yml。
pub fn toggle_ids(profile: &str, ids: &[String], enabled: bool) -> Result<(), String> {
    toggle_ids_at(&patch_file(profile), ids, enabled)
}

/// `toggle_ids` 的落地版本：直接对某个 `cordis.patch.yml` 生效（便于测试）。
pub fn toggle_ids_at(path: &Path, ids: &[String], enabled: bool) -> Result<(), String> {
    if ids.is_empty() {
        return Err("该插件没有可启停的条目 id".to_string());
    }
    let path: PathBuf = path.to_path_buf();
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    // 一个包可能 insert 了多行（如 @michengai/dsh-archive-manager 的三行），
    // 必须一起改：只关一行会留下半个插件在跑。
    // 改写失败（形态不支持）时直接返回错误，绝不落盘半个坏文件
    let mut updated = original.clone();
    for id in ids {
        updated = set_enabled_in_patch(&updated, id, enabled)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    // 留一份备份，改坏了可以回退
    if !original.is_empty() {
        let _ = std::fs::write(Path::new(&format!("{}.bak", path.display())), &original);
    }
    std::fs::write(&path, updated).map_err(|e| format!("写入 {} 失败: {}", path.display(), e))?;
    config::log(&format!(
        "plugin ids [{}] set enabled={} in {}",
        ids.join(", "),
        enabled,
        path.display()
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_spec_strips_prefix() {
        assert_eq!(install_spec("dsh plugin --profile web add dsh-pet"), "dsh-pet");
        assert_eq!(
            install_spec("dsh plugin --profile web add github:a/b"),
            "github:a/b"
        );
        assert_eq!(install_spec("dsh-pet"), "dsh-pet");
    }

    #[test]
    fn disabled_detection() {
        let patch = "\
- id: ui-settings-plugin-inventory
  disabled: true
- id: other
  disabled: false
";
        assert!(is_disabled_in_patch(patch, "ui-settings-plugin-inventory"));
        assert!(!is_disabled_in_patch(patch, "other"));
        assert!(!is_disabled_in_patch(patch, "not-there"));
    }

    /// 改写结果必须始终是合法 YAML，且顶层是数组。
    fn assert_valid(md: &str) -> Vec<serde_yaml::Value> {
        let v: serde_yaml::Value = serde_yaml::from_str(md).expect("改写后必须是合法 YAML");
        v.as_sequence().expect("顶层必须是数组").clone()
    }

    #[test]
    fn toggle_flips_existing_without_touching_comments() {
        let patch = "\
# 顶部注释
- id: a
  disabled: true

- id: b
  disabled: false
";
        let out = set_enabled_in_patch(patch, "a", true).unwrap();
        assert!(out.contains("# 顶部注释"), "注释必须保留");
        assert!(!is_disabled_in_patch(&out, "a"), "a 应变为启用");
        assert!(!is_disabled_in_patch(&out, "b"), "b 不受影响");
        assert_eq!(assert_valid(&out).len(), 2);

        let out2 = set_enabled_in_patch(&out, "b", false).unwrap();
        assert!(is_disabled_in_patch(&out2, "b"));
        assert!(!is_disabled_in_patch(&out2, "a"));
    }

    #[test]
    fn toggle_appends_when_missing() {
        let patch = "- id: a\n  disabled: false\n";
        let out = set_enabled_in_patch(patch, "brand-new", false).unwrap();
        assert!(is_disabled_in_patch(&out, "brand-new"), "新条目应写入 disabled: true");
        assert!(!is_disabled_in_patch(&out, "a"));
        assert_eq!(assert_valid(&out).len(), 2);
    }

    #[test]
    fn toggle_inserts_disabled_line_when_absent() {
        let patch = "- id: a\n";
        let out = set_enabled_in_patch(patch, "a", false).unwrap();
        assert!(is_disabled_in_patch(&out, "a"));
        assert!(out.contains("- id: a"));
        assert_eq!(assert_valid(&out).len(), 1);
    }

    /// 回归：DSH 生成的默认 patch 就是「注释 + 一行 []」。
    /// 老实现会写成 `[]` 后面再跟块序列 → 非法 YAML，DSH 下次启动直接读不了 profile。
    #[test]
    fn toggle_on_shipped_default_patch_stays_valid_yaml() {
        let patch = "# 你的补丁层\n# 注释若干\n[]\n";
        let out = set_enabled_in_patch(patch, "dsh-pet", false).unwrap();
        assert!(!out.contains("[]"), "空的流式数组必须被移除：\n{}", out);
        assert!(out.contains("# 你的补丁层"), "注释仍然保留");
        let seq = assert_valid(&out);
        assert_eq!(seq.len(), 1);
        assert_eq!(seq[0].get("id").and_then(|x| x.as_str()), Some("dsh-pet"));
        assert_eq!(seq[0].get("disabled").and_then(|x| x.as_bool()), Some(true));
        assert!(is_disabled_in_patch(&out, "dsh-pet"));

        // 再启用一次：原地改回来，仍然合法
        let back = set_enabled_in_patch(&out, "dsh-pet", true).unwrap();
        assert!(!is_disabled_in_patch(&back, "dsh-pet"));
        assert_eq!(assert_valid(&back).len(), 1);
    }

    /// 只有注释、没有文档内容的空文件也要能安全追加。
    #[test]
    fn toggle_on_comment_only_patch_stays_valid() {
        let patch = "# 只有注释\n";
        let out = set_enabled_in_patch(patch, "x", false).unwrap();
        assert!(is_disabled_in_patch(&out, "x"));
        assert_eq!(assert_valid(&out).len(), 1);
    }

    /// 非空的流式数组不自动改写：宁可不写，也不写坏。
    #[test]
    fn toggle_refuses_nonempty_flow_style() {
        let patch = "[{ id: a, disabled: false }]\n";
        assert!(set_enabled_in_patch(patch, "b", false).is_err());
    }

    #[test]
    fn category_labels_are_chinese_with_passthrough() {
        assert_eq!(category_label("ui"), "界面");
        assert_eq!(category_label("tools"), "工具");
        assert_eq!(category_label("workflow"), "工作流");
        // 分类条里的每一个键都必须有映射，漏一个就会在小写英文上露馅
        for key in [
            "ui", "theme", "fun", "tools", "model", "usage", "session", "memory", "notify",
            "workflow", "git", "docs", "voice", "vision", "security", "market", "github",
        ] {
            assert_ne!(category_label(key), key, "分类 {} 缺少中文映射", key);
        }
        // GitHub 是专有名词，按惯例写
        assert_eq!(category_label("github"), "GitHub");
        // 市场随时可能加新分类：未知键原样返回，不能变空白
        assert_eq!(category_label("brand-new-cat"), "brand-new-cat");
    }

    #[test]
    fn filter_by_category_and_query() {
        let list = vec![
            PluginInfo { name: "dsh-pet".into(), category: "ui".into(), ..Default::default() },
            PluginInfo {
                name: "dsh-meme".into(),
                category: "fun".into(),
                description_zh: "表情包".into(),
                ..Default::default()
            },
        ];
        assert_eq!(filter_local(&list, "", "ui").len(), 1);
        assert_eq!(filter_local(&list, "meme", "").len(), 1);
        assert_eq!(filter_local(&list, "表情", "").len(), 1);
        assert_eq!(filter_local(&list, "zzz", "").len(), 0);
    }

    // ---------------------------------------------------------- 多途径扫描

    /// 造一棵 fixture：`<root>/home`（DSH_HOME）+ `<root>/install/…/@deepseek-ai/dsh`。
    fn fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("dsh-plugin-scan-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn put(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// 一棵"真机上会长成什么样"的目录树：登记过的依赖、pnpm 装的、传递依赖、
    /// 只带 cordis.patch.yml 的老式插件、dsh 自带层、以及补丁里的孤立 id。
    fn build_fixture(root: &Path) -> ScanContext {
        let home = root.join("home");
        let install = root.join("install").join("node_modules").join("@deepseek-ai").join("dsh");
        put(
            &install.join("package.json"),
            r#"{"name":"@deepseek-ai/dsh","version":"9.9.9","dependencies":{"@deepseek-ai/dsh-base":"1.0.0"}}"#,
        );
        put(
            &home.join("profiles").join("web").join("package.json"),
            r#"{
              "name":"dsh-profile-web","private":true,
              "dependencies":{"plugin-a":"^1.2.0","@scope/plugin-b":"2.0.0"},
              "dsh":{"profile":{"bundles":["plugin-a","@scope/plugin-b","@deepseek-ai/dsh-base"]}}
            }"#,
        );
        put(
            &home.join("profiles").join("web").join("cordis.patch.yml"),
            "# 用户补丁\n- id: orphan-id\n  disabled: true\n",
        );
        // 1) 登记过的依赖：dsh.bundle.patch 指向随包补丁
        put(
            &home.join("profiles/web/node_modules/plugin-a/package.json"),
            r#"{"name":"plugin-a","version":"1.2.3","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        );
        put(
            &home.join("profiles/web/node_modules/plugin-a/cordis.patch.yml"),
            "- insert:\n    - id: plugin-a-layer\n      name: plugin-a\n",
        );
        // 2) 作用域包：没写 dsh.bundle，只随包带 cordis.patch.yml（老形态）
        put(
            &home.join("profiles/web/node_modules/@scope/plugin-b/package.json"),
            r#"{"name":"@scope/plugin-b","version":"2.0.0"}"#,
        );
        put(
            &home.join("profiles/web/node_modules/@scope/plugin-b/cordis.patch.yml"),
            "- insert:\n    - id: b-layer\n      name: '@scope/plugin-b'\n",
        );
        // 3) 传递依赖：必须被排除
        put(
            &home.join("profiles/web/node_modules/js-yaml/package.json"),
            r#"{"name":"js-yaml","version":"4.3.2"}"#,
        );
        // 4) dsh 自带的层：不是用户装的。
        //    它的补丁里 insert 了一整个底座（真机 dsh-base 就是上百行），
        //    绝不能把这些 id 当成"这个插件可启停的条目"。
        put(
            &home.join("profiles/node_modules/@deepseek-ai/dsh-base/package.json"),
            r#"{"name":"@deepseek-ai/dsh-base","version":"1.0.0","dsh":{"bundle":{"patch":"./cordis.patch.yml"}}}"#,
        );
        put(
            &home.join("profiles/node_modules/@deepseek-ai/dsh-base/cordis.patch.yml"),
            "- id: tools\n  disabled: false\n- insert:\n    - id: base-a\n      name: '@deepseek-ai/dsh-llm'\n    - id: base-b\n      name: '@deepseek-ai/dsh-tools'\n    - id: base-c\n      name: '@deepseek-ai/dsh-web-app'\n    - id: base-d\n      name: '@deepseek-ai/dsh-agent'\n    - id: base-e\n      name: '@deepseek-ai/dsh-session'\n    - id: base-f\n      name: '@deepseek-ai/dsh-jobs'\n",
        );
        ScanContext { home, install: Some(install) }
    }

    #[test]
    fn plugin_ids_come_from_insert_rows_only() {
        let root = fixture("ids");
        let bloom = root.join("bloom");
        put(
            &bloom.join("cordis.patch.yml"),
            "# 注释\n- insert:\n    - id: bloom-theme\n      name: \"dsh-bloom-theme\"\n",
        );
        put(&bloom.join("package.json"), r#"{"name":"dsh-bloom-theme"}"#);
        assert_eq!(plugin_ids(&bloom, "cordis.patch.yml", "dsh-bloom-theme"), vec!["bloom-theme"]);

        // 一个包 insert 多行：三行都要能启停（@michengai/dsh-archive-manager 的真实形态）
        let arch = root.join("arch");
        put(
            &arch.join("cordis.patch.yml"),
            "- id: workspace\n  disabled: true\n- insert:\n    - id: workspace-archive-manager\n      name: '@michengai/dsh-archive-manager/workspace'\n    - id: session-projection-cache-archive-manager\n      name: '@michengai/dsh-archive-manager/projcache'\n    - id: ui-workspace-archive-manager\n      name: '@michengai/dsh-archive-manager'\n",
        );
        assert_eq!(
            plugin_ids(&arch, "cordis.patch.yml", "@michengai/dsh-archive-manager"),
            vec![
                "session-projection-cache-archive-manager",
                "ui-workspace-archive-manager",
                "workspace-archive-manager"
            ]
        );

        // 官方 bundle 形态：只有 id 覆盖、没有 insert → 不给启停入口
        // （老实现取"第一个 id"会返回 tools，一禁用就把系统组件关掉）
        // 模块名写成相对路径的小插件补丁：没有包名前缀可匹配，按"它自己的层"认
        let rel = root.join("rel");
        put(
            &rel.join("cordis.patch.yml"),
            "- insert:\n    - id: rel-layer\n      name: './lib/index.js'\n",
        );
        assert_eq!(plugin_ids(&rel, "cordis.patch.yml", "some-plugin"), vec!["rel-layer"]);

        let webapp = root.join("webapp");
        put(
            &webapp.join("cordis.patch.yml"),
            "- id: system-prompt\n  config:\n    personaSuffix: x\n- id: tools\n  disabled: false\n",
        );
        assert!(plugin_ids(&webapp, "cordis.patch.yml", "@deepseek-ai/dsh-web-app").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn top_level_ids_ignore_insert_children() {
        let patch = "# c\n- id: a\n  disabled: false\n- insert:\n    - id: b\n      name: pkg\n";
        assert_eq!(top_level_patch_ids(patch), vec!["a"]);
        assert_eq!(top_level_patch_ids("[]\n"), Vec::<String>::new());
    }

    #[test]
    fn scan_merges_every_route_and_skips_transitive_deps() {
        let root = fixture("scan");
        let ctx = build_fixture(&root);
        let rep = scan_with(&ctx, "web");

        let names: Vec<&str> = rep.plugins.iter().map(|p| p.package.as_str()).collect();
        assert_eq!(names, vec!["@scope/plugin-b", "plugin-a"], "传递依赖 js-yaml 不该出现");

        let a = &rep.plugins[1];
        assert_eq!(a.version, "1.2.3", "版本要用磁盘上真实的那份");
        assert_eq!(a.ids, vec!["plugin-a-layer"]);
        assert!(a.enabled);
        assert!(a.layer);
        assert!(a.routes.contains(&PluginRoute::Dependency));
        assert!(a.routes.contains(&PluginRoute::BundleLayer));
        assert!(a.routes.contains(&PluginRoute::ProfileModules));

        // 只随包带 cordis.patch.yml 的作用域包也要能扫到、能启停
        let b = &rep.plugins[0];
        assert_eq!(b.version, "2.0.0");
        assert_eq!(b.ids, vec!["b-layer"]);
        assert!(b.layer);
        assert!(b.routes.contains(&PluginRoute::BundleLayer));

        // dsh 自带层单独归类
        assert_eq!(rep.builtin.len(), 1);
        assert_eq!(rep.builtin[0].package, "@deepseek-ai/dsh-base");
        assert_eq!(rep.builtin[0].origin, PluginOrigin::Installation);
        assert!(
            rep.builtin[0].ids.is_empty(),
            "官方底座（insert 上百行）绝不能给出可启停 id，否则一次误点就把 DSH 拆了"
        );
        assert!(!rep.builtin[0].patch_only);

        // 补丁里的孤立 id
        assert_eq!(rep.patch_rows.len(), 1);
        assert_eq!(rep.patch_rows[0].package, "orphan-id");
        assert!(rep.patch_rows[0].patch_only);
        assert!(!rep.patch_rows[0].enabled, "补丁里写了 disabled: true");

        // 途径计数与查过的目录
        let count = |r: PluginRoute| rep.counts.iter().find(|(rr, _)| *rr == r).unwrap().1;
        assert_eq!(count(PluginRoute::Dependency), 2, "plugin-a 与 @scope/plugin-b 都登记在 dependencies");

        assert_eq!(count(PluginRoute::BundleLayer), 3);
        assert_eq!(count(PluginRoute::ProfileModules), 2);
        assert_eq!(count(PluginRoute::Installation), 1);
        assert_eq!(count(PluginRoute::PatchOnly), 1);
        assert!(rep.roots.iter().any(|r| r.contains("profiles")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_survives_missing_profile() {
        let root = fixture("missing");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let rep = scan_with(&ScanContext { home, install: None }, "web");
        assert!(rep.plugins.is_empty());
        // 目录不存在 + 没解析到 dsh 安装：两条提示，界面折叠显示
        assert_eq!(rep.warnings.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真实 DSH_HOME 冒烟（只读）：`cargo test -- --ignored --nocapture`
    #[test]
    #[ignore = "读真实 DSH_HOME，人工跑：cargo test -- --ignored --nocapture"]
    fn scan_real_home_smoke() {
        let ctx = ScanContext::detect();
        let profile = std::env::var("DSH_LAUNCH_CONSOLE_PROFILE").unwrap_or_else(|_| "web".into());
        let rep = scan_with(&ctx, &profile);
        println!("home={:?} install={:?}", ctx.home, ctx.install);
        println!("roots: {:#?}", rep.roots);
        println!("counts: {:?}", rep.counts);
        for p in &rep.plugins {
            println!(
                "  [插件] {} {} ids={:?} enabled={} routes={:?}",
                p.package,
                p.version,
                p.ids,
                p.enabled,
                p.routes.iter().map(|r| r.label()).collect::<Vec<_>>()
            );
        }
        for p in &rep.builtin {
            println!("  [自带] {} {} enabled={}", p.package, p.version, p.enabled);
        }
        for p in &rep.patch_rows {
            println!("  [补丁] {} enabled={}", p.package, p.enabled);
        }
        for w in &rep.warnings {
            println!("  [提示] {}", w);
        }
    }

    /// 落盘版：真的写文件、留备份、改完仍是合法 YAML。
    #[test]
    fn toggle_ids_at_writes_profile_patch_with_backup() {
        let root = fixture("toggle-write");
        let patch = root.join("profiles").join("web").join("cordis.patch.yml");
        put(&patch, "# 用户补丁\n- id: bloom-theme\n  disabled: false\n");
        let ids = vec!["bloom-theme".to_string()];
        toggle_ids_at(&patch, &ids, false).unwrap();
        let after = std::fs::read_to_string(&patch).unwrap();
        assert!(is_disabled_in_patch(&after, "bloom-theme"));
        assert!(after.contains("# 用户补丁"), "注释必须保留");
        assert_eq!(assert_valid(&after).len(), 1, "原地改，不新增条目");
        assert!(root.join("profiles/web/cordis.patch.yml.bak").is_file(), "改前留一份备份");
        // 空 id 列表直接拒绝，不写文件
        assert!(toggle_ids_at(&patch, &[], true).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 一个包多个 id 时，启停必须一次改完，且改完仍是合法 YAML。
    #[test]
    fn toggle_ids_writes_every_row() {
        let patch = "# 用户补丁\n[]\n";
        let mut out = patch.to_string();
        let ids = vec!["a-layer".to_string(), "b-layer".to_string()];
        for id in &ids {
            out = set_enabled_in_patch(&out, id, false).unwrap();
        }
        assert!(is_disabled_in_patch(&out, "a-layer"));
        assert!(is_disabled_in_patch(&out, "b-layer"));
        assert!(out.contains("# 用户补丁"));
        assert_eq!(assert_valid(&out).len(), 2);
        // 再一起启用：原地改回，条目数不变
        let back = ids.iter().fold(out.clone(), |acc, id| {
            set_enabled_in_patch(&acc, id, true).unwrap()
        });
        assert!(!is_disabled_in_patch(&back, "a-layer"));
        assert!(!is_disabled_in_patch(&back, "b-layer"));
        assert_eq!(assert_valid(&back).len(), 2);
    }
}

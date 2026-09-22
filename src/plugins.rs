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

/// 已安装插件（从 profile 的 package.json + cordis.patch.yml 推导）。
#[derive(Debug, Clone, Default)]
pub struct InstalledPlugin {
    /// package.json 里的依赖名
    pub package: String,
    /// cordis 里的插件 id（启停用）
    pub id: Option<String>,
    pub version: String,
    pub enabled: bool,
    /// 是否来自 bundles 列表（DSH 自带/主题类，仅用于打标签）
    pub bundled: bool,
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

/// 读取已安装插件列表。
pub fn installed(profile: &str) -> Vec<InstalledPlugin> {
    let mut out: Vec<InstalledPlugin> = Vec::new();
    let pj = profile_package_json(profile);
    let Ok(txt) = std::fs::read_to_string(&pj) else {
        return out;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return out;
    };

    let bundles: Vec<String> = doc
        .get("dsh")
        .and_then(|d| d.get("profile"))
        .and_then(|p| p.get("bundles"))
        .and_then(|b| b.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let deps = doc.get("dependencies").and_then(|d| d.as_object());
    let patch = std::fs::read_to_string(patch_file(profile)).unwrap_or_default();

    // 依赖里出现的插件
    if let Some(deps) = deps {
        for (name, ver) in deps {
            let id = plugin_id_of(profile, name);
            let enabled = id
                .as_deref()
                .map(|i| !is_disabled_in_patch(&patch, i))
                .unwrap_or(true);
            out.push(InstalledPlugin {
                package: name.clone(),
                id,
                version: ver.as_str().unwrap_or("").to_string(),
                enabled,
                bundled: bundles.iter().any(|b| b == name),
            });
        }
    }
    // 注意：只列 profile package.json 里 `dependencies` 真正装了的插件。
    // DSH 自带的 bundle（dsh-base / dsh-web-app 这些）不是用户装的插件，
    // 不在这里显示——插件页只管用户装的东西。
    out.sort_by(|a, b| a.package.cmp(&b.package));
    out
}

/// 在 profile 里定位某个包的目录。
///
/// 依次尝试：
/// 1. `<profile>/node_modules/<pkg>`（npm / 直接安装）
/// 2. `<profile>/node_modules/.pnpm/<escaped>@<ver>/node_modules/<pkg>`
///    （pnpm 的真实内容目录——这是 dsh plugin add 的实际落点）
fn package_dir(profile: &str, package: &str) -> Option<PathBuf> {
    let nm = profile_dir(profile).join("node_modules");
    let direct = nm.join(package);
    if direct.join("package.json").is_file() {
        return Some(direct);
    }
    let escaped = package.replace('/', "+");
    let rd = std::fs::read_dir(nm.join(".pnpm")).ok()?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&format!("{}@", escaped)) {
            continue;
        }
        let cand = e.path().join("node_modules").join(package);
        if cand.join("package.json").is_file() {
            return Some(cand);
        }
    }
    None
}

/// 求某个 npm 包在 cordis 里注册的插件 id：读该包自己的补丁文件，取第一个 `id:`。
///
/// **bundle 不算插件**：`@deepseek-ai/dsh-web-app` 这类 bundle 的补丁里有几十行
/// 按行 id 覆盖的子系统（`system-prompt`、`tools`…），取第一个 id 会把系统组件
/// 当成插件关掉。因此 package.json 里带 `dsh.bundle` 的一律返回 None（界面显示
/// 为不可单独启停），只有真正的插件包才解析 id。
fn plugin_id_of(profile: &str, package: &str) -> Option<String> {
    let dir = package_dir(profile, package)?;
    let pj = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
    if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&pj) {
        let dsh = doc.get("dsh");
        if dsh.and_then(|d| d.get("bundle")).is_some() {
            return None;
        }
    }
    let txt = std::fs::read_to_string(dir.join("cordis.patch.yml")).ok()?;
    for line in txt.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("- id:") {
            return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
        }
        if let Some(rest) = t.strip_prefix("id:") {
            return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
        }
    }
    None
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
pub fn toggle(profile: &str, id: &str, enabled: bool) -> Result<(), String> {
    let path: PathBuf = patch_file(profile);
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    // 改写失败（形态不支持）时直接返回错误，绝不落盘半个坏文件
    let updated = set_enabled_in_patch(&original, id, enabled)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {}", e))?;
    }
    // 留一份备份，改坏了可以回退
    if !original.is_empty() {
        let _ = std::fs::write(Path::new(&format!("{}.bak", path.display())), &original);
    }
    std::fs::write(&path, updated)
        .map_err(|e| format!("写入 {} 失败: {}", path.display(), e))?;
    config::log(&format!(
        "plugin {} set enabled={} in {}",
        id,
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
}

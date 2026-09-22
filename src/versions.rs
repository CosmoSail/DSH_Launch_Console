#![allow(dead_code)] // 平台相关/预留 API
//! DSH 版本管理：检索 npm 上 `@deepseek-ai/dsh` 的全部版本，并把
//! **全局安装**切换/安装到选定版本（`npm install -g @deepseek-ai/dsh@<version>`）。
//!
//! 不再有「启动器自管版本目录」：DSH 只有一份，就是全局那一份。
//! 版本页负责查询可装版本、显示当前全局版本、以及把全局版本切成另一个。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::procs;

/// 一个可选版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub version: String,
    /// npm dist-tag（latest / next / alpha…），无则为空
    #[serde(default)]
    pub tag: String,
    /// 发布日期（ISO8601）
    #[serde(default)]
    pub published: String,
}

/// 版本目录快照。
#[derive(Debug, Clone, Default)]
pub struct VersionList {
    pub versions: Vec<VersionInfo>,
    /// 最新稳定版本（dist-tags.latest）
    pub latest: String,
    /// 所有 dist-tags
    pub tags: Vec<(String, String)>,
    /// 抓取时间（unix 秒）
    pub fetched_at: u64,
}

/// 本地已装版本（就是全局安装那一份）。
#[derive(Debug, Clone, Default)]
pub struct Installed {
    /// 全局 `npm i -g` 装的版本号
    pub global: Option<String>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 只读缓存的路径（抓到的版本列表会落盘，下次启动先用缓存秒开界面）。
fn cache_file() -> PathBuf {
    config::data_dir().join("DSH-Launch-Console-versions.json")
}

/// 读取磁盘缓存。
pub fn load_cache() -> Option<VersionList> {
    let txt = std::fs::read_to_string(cache_file()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
    let mut out = VersionList::default();
    out.latest = v.get("latest").and_then(|x| x.as_str()).unwrap_or("").to_string();
    out.fetched_at = v.get("fetched_at").and_then(|x| x.as_u64()).unwrap_or(0);
    if let Some(tags) = v.get("tags").and_then(|x| x.as_object()) {
        out.tags = tags
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect();
        out.tags.sort();
    }
    if let Some(arr) = v.get("versions").and_then(|x| x.as_array()) {
        for item in arr {
            out.versions.push(VersionInfo {
                version: item.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                tag: item.get("tag").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                published: item
                    .get("published")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    if out.versions.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn save_cache(list: &VersionList) {
    let tags: serde_json::Map<String, serde_json::Value> = list
        .tags
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    let versions: Vec<serde_json::Value> = list
        .versions
        .iter()
        .map(|v| {
            serde_json::json!({
                "version": v.version,
                "tag": v.tag,
                "published": v.published,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "latest": list.latest,
        "tags": serde_json::Value::Object(tags),
        "fetched_at": list.fetched_at,
        "versions": versions,
    });
    let _ = std::fs::write(cache_file(), serde_json::to_string_pretty(&doc).unwrap_or_default());
}

/// 从 npm registry 抓取版本列表（阻塞，调用方应放在后台线程）。
pub fn fetch_remote() -> Result<VersionList, String> {
    let url = format!("https://registry.npmjs.org/{}", config::DSH_PACKAGE);
    let body: serde_json::Value = config::http_agent()
        .get(&url)
        .header("Accept", "application/vnd.npm.install-v1+json, application/json")
        .call()
        .map_err(|e| format!("无法访问 npm registry: {}", e))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("npm registry 返回内容无法解析: {}", e))?;

    let mut tags: Vec<(String, String)> = Vec::new();
    if let Some(t) = body.get("dist-tags").and_then(|x| x.as_object()) {
        for (k, v) in t {
            tags.push((k.clone(), v.as_str().unwrap_or("").to_string()));
        }
        tags.sort();
    }
    let latest = body
        .get("dist-tags")
        .and_then(|x| x.get("latest"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    // 版本号 → 该版本命中的 dist-tag
    let tag_of = |ver: &str| -> String {
        tags.iter()
            .find(|(_, v)| v == ver)
            .map(|(k, _)| k.clone())
            .unwrap_or_default()
    };

    let mut versions: Vec<VersionInfo> = Vec::new();
    if let Some(map) = body.get("versions").and_then(|x| x.as_object()) {
        for (ver, _) in map {
            versions.push(VersionInfo {
                version: ver.clone(),
                tag: tag_of(ver),
                published: String::new(),
            });
        }
    }
    // 发布时间（完整 packument 才有；拿不到就留空）
    if let Some(times) = body.get("time").and_then(|x| x.as_object()) {
        for v in versions.iter_mut() {
            if let Some(t) = times.get(&v.version).and_then(|x| x.as_str()) {
                v.published = t.to_string();
            }
        }
    }

    versions.sort_by(|a, b| compare_versions(&b.version, &a.version));

    let list = VersionList { versions, latest, tags, fetched_at: now_secs() };
    if !list.versions.is_empty() {
        save_cache(&list);
    }
    Ok(list)
}

/// 语义化版本比较（数字段优先，预发布版小于同号正式版）。
///
/// 预发布段按 semver 规则逐段比较：纯数字段按数值比，其余按字典序；
/// 数字段小于非数字段；段数少的小于段数多的（如 `rc` < `rc.1`）。
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    fn split(v: &str) -> (Vec<u64>, Vec<String>) {
        // 构建元数据（+ 之后）不参与比较
        let v = v.split('+').next().unwrap_or(v);
        let (core, pre) = match v.split_once('-') {
            Some((c, p)) => (c, p),
            None => (v, ""),
        };
        let nums = core
            .split('.')
            .map(|s| s.trim().parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>();
        let pre = if pre.is_empty() {
            Vec::new()
        } else {
            pre.split('.').map(|s| s.to_string()).collect()
        };
        (nums, pre)
    }

    fn cmp_pre(pa: &[String], pb: &[String]) -> Ordering {
        match (pa.is_empty(), pb.is_empty()) {
            (true, true) => return Ordering::Equal,
            // 有预发布段的小于无预发布段（正式版更大）
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            (false, false) => {}
        }
        for i in 0..pa.len().max(pb.len()) {
            match (pa.get(i), pb.get(i)) {
                (Some(x), Some(y)) => {
                    let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                        // 都是数字：按数值比（rc.10 > rc.9）
                        (Ok(a), Ok(b)) => a.cmp(&b),
                        // 数字段 < 非数字段
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        // 都是非数字：字典序
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                // 段数少的小（rc < rc.1）
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (None, None) => break,
            }
        }
        Ordering::Equal
    }

    let (na, pa) = split(a);
    let (nb, pb) = split(b);
    for i in 0..na.len().max(nb.len()) {
        let x = na.get(i).copied().unwrap_or(0);
        let y = nb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    cmp_pre(&pa, &pb)
}

/// 扫描全局安装的 DSH 版本。
pub fn installed() -> Installed {
    let mut out = Installed::default();
    for root in crate::dsh::global_roots() {
        let pj = root.join("node_modules").join("@deepseek-ai").join("dsh").join("package.json");
        if let Ok(txt) = std::fs::read_to_string(&pj) {
            if let Some(v) = version_field(&txt) {
                out.global = Some(v);
                break;
            }
        }
    }
    out
}

/// 从 package.json 文本里取 version（只做最小解析，避免引 serde 反序列化整个包描述）。
pub fn version_field(txt: &str) -> Option<String> {
    let idx = txt.find("\"version\"")?;
    let after = &txt[idx + 9..];
    let rest = after.get(after.find(':')? + 1..)?.trim_start();
    let s = rest.strip_prefix('"')?;
    Some(s[..s.find('"')?].to_string())
}

/// npm 可执行文件路径。
fn npm_cmd() -> Result<PathBuf, String> {
    crate::dsh::which("npm").ok_or_else(|| {
        "未找到 npm。DSH 是 Node 应用，请先安装 Node.js（自带 npm）。".to_string()
    })
}

/// 把**全局安装**切换/安装到指定版本（阻塞，调用方放后台线程）。
///
/// 就是 `npm install -g @deepseek-ai/dsh@<version>`：npm 会就地替换全局那一份，
/// 所以「装某个版本」和「切换到某个版本」是同一个动作。
///
/// 装完会回头验证一次「启动器还能解析到全局入口」，避免 npm 全局前缀不在
/// 检测范围内时界面显示成功、实际却起不来。
pub fn install_global(version: &str, log: &mut dyn FnMut(String)) -> Result<String, String> {
    let npm = npm_cmd()?;
    let spec = format!("{}@{}", config::DSH_PACKAGE, version);
    log(format!("正在把全局安装切换为 {} …", spec));

    let out = procs::hidden_command(&npm)
        .arg("install")
        .arg("-g")
        .arg("--no-audit")
        .arg("--no-fund")
        .arg("--loglevel=error")
        .arg(&spec)
        .output()
        .map_err(|e| format!("执行 npm 失败: {}", e))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines().chain(stdout.lines()).filter(|l| !l.trim().is_empty()) {
        log(format!("  {}", line));
    }

    if !out.status.success() {
        return Err(format!(
            "安装失败（npm 退出码 {}）。\n可尝试在终端手动执行：\n  npm i -g {}",
            out.status.code().unwrap_or(-1),
            spec
        ));
    }

    // 回头验证：全局入口必须还能解析到
    match crate::dsh::resolve_entry() {
        Ok(e) => {
            let now = installed().global.unwrap_or_else(|| "未知".to_string());
            log(format!("全局安装现在是 {}（{}）", now, e.entry.display()));
            if now != version {
                log(format!(
                    "提示：npm 报告的版本是 {}，与请求的 {} 不一致，请确认全局前缀与 registry",
                    now, version
                ));
            }
        }
        Err(e) => {
            return Err(format!(
                "安装完成，但启动器仍解析不到全局入口，请检查 npm 全局前缀：\n{}",
                e
            ))
        }
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        use std::cmp::Ordering;
        assert_eq!(compare_versions("0.1.5", "0.1.4"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.10", "0.1.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
        // 预发布小于同号正式版
        assert_eq!(compare_versions("0.1.5-rc.2", "0.1.5"), Ordering::Less);
        assert_eq!(compare_versions("0.1.5", "0.1.5-rc.2"), Ordering::Greater);
        // 预发布序号按数值比（不是字典序）
        assert_eq!(compare_versions("0.1.5-rc.10", "0.1.5-rc.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.5-alpha.10", "0.1.5-alpha.2"), Ordering::Greater);
        // alpha < rc（字典序）
        assert_eq!(compare_versions("0.1.5-alpha.2", "0.1.5-rc.1"), Ordering::Less);
        // 段数少的小
        assert_eq!(compare_versions("0.1.5-rc", "0.1.5-rc.1"), Ordering::Less);
        // 构建元数据不参与比较
        assert_eq!(compare_versions("0.1.5+build1", "0.1.5+build2"), Ordering::Equal);
    }

    #[test]
    fn version_field_parse() {
        assert_eq!(
            version_field(r#"{"name":"@deepseek-ai/dsh","version":"0.1.6-alpha.2"}"#).as_deref(),
            Some("0.1.6-alpha.2")
        );
        assert_eq!(version_field(r#"{"name":"x"}"#), None);
    }
}

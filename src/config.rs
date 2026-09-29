//! 路径与运行期配置。
//!
//! 小文件（设置、版本列表缓存、启动器日志）都放系统临时目录，不新建目录。
//!
//! **例外：0.3.1 起的「自管版本目录」**。多实例要能跑不同版本的 DSH，就得有地方
//! 放这些版本——放在临时目录里会被系统清理掉，于是重新用回
//! `%LOCALAPPDATA%\DSH-Launch-Console`（仅在真的安装第二个版本时才创建）。
//! 注意这**不是** 0.2.0 之前那个「自管版本目录」：那时它是启动 DSH 的唯一来源；
//! 现在它只服务于「另装一个版本」这件事，全局安装那一份照旧原样使用。

use std::path::PathBuf;

/// 数据目录：直接用系统临时目录，**不创建任何目录**。
pub fn data_dir() -> PathBuf {
    std::env::temp_dir()
}

/// 启动器自己的持久化根目录（**唯一会主动创建目录的地方**）。
///
/// 放 `%LOCALAPPDATA%\DSH-Launch-Console`（Unix：`$XDG_DATA_HOME` 或 `~/.local/share`），
/// 因为自管的 DSH 版本按设计要长期留着，不能放临时目录。
/// 可用 `DSH_LAUNCH_CONSOLE_DATA` 覆盖（测试用独立目录）。
pub fn app_data_dir() -> PathBuf {
    if let Some(v) = env_nonempty("DSH_LAUNCH_CONSOLE_DATA") {
        return PathBuf::from(v);
    }
    #[cfg(windows)]
    {
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(l).join("DSH-Launch-Console");
        }
    }
    #[cfg(unix)]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
            if !x.is_empty() {
                return PathBuf::from(x).join("DSH-Launch-Console");
            }
        }
        return home_dir().join(".local").join("share").join("DSH-Launch-Console");
    }
    #[allow(unreachable_code)]
    data_dir().join("DSH-Launch-Console")
}

/// 某个实例自己的目录：`<app_data>/instances/<实例 id>`。
///
/// 自建实例的一切都放这里面——它的 DSH_HOME 与它自己的 DSH 版本。
/// **全部收在一个目录下**，是为了让"删掉这个实例"变成一次目录删除：
/// 不用担心漏掉什么残留在别处（以前 home 在 `homes/`、版本在共用的 `versions/`，
/// 删实例只删配置，磁盘上会越留越多）。
pub fn instance_dir(id: &str) -> PathBuf {
    app_data_dir().join("instances").join(safe_slug(id))
}

/// 某个实例的自管版本根目录：`<app_data>/instances/<id>/versions`。
///
/// 注意这里**按实例分开**：自建实例之间不共用版本文件。
/// 代价是同一个版本被两个实例用到时会各存一份；换来的是
/// "删掉某个实例 = 删掉它那一整个目录"，不必去猜哪个版本还有别人在用。
pub fn instance_versions_dir(id: &str) -> PathBuf {
    instance_dir(id).join("versions")
}

/// 某个实例里某个版本的安装目录：`<app_data>/instances/<id>/versions/<版本>`。
///
/// 版本号直接当目录名，所以它**必须**先过 [`is_safe_version`]——
/// 这是要拼进路径的，不能让 `..` 之类的东西进来。
pub fn instance_version_dir(id: &str, version: &str) -> PathBuf {
    instance_versions_dir(id).join(version)
}

/// 旧版（0.3.1 开发期）共用版本库的根目录：`<app_data>/versions`。
///
/// 只在**迁移**里用得到：把老的共用版本搬进各实例自己的目录。
/// 新代码不再往这里写任何东西。
pub fn legacy_versions_dir() -> PathBuf {
    app_data_dir().join("versions")
}

/// 旧版每个实例 DSH_HOME 的根目录：`<app_data>/homes/<id>`。
/// 同样只用于迁移到 [`instance_dir`]。
pub fn legacy_home_dir(id: &str) -> PathBuf {
    app_data_dir().join("homes").join(safe_slug(id))
}

/// 删掉某个实例的整个目录（它的 DSH_HOME + 它自己装的 DSH 版本）。
///
/// 这是「删实例 = 数据一起清掉」的落地：因为实例的一切都收在
/// [`instance_dir`] 下面，这里只需要删一个目录，不必逐个去猜哪些版本还有别人在用。
///
/// **不可逆**，调用方必须先确认。删不掉时返回错误（比如实例正跑着、
/// Windows 上文件被占用），而不是假装成功——否则界面上"已删除"但其实还占着几百 MB。
pub fn remove_instance_dir(id: &str) -> Result<(), String> {
    let dir = instance_dir(id);
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).map_err(|e| {
        format!(
            "删不掉实例目录 {}：{}\n（若实例正在运行，请先关闭它再删）",
            dir.display(),
            e
        )
    })
}

/// 版本号能不能安全地当路径片段用。
///
/// 只接受数字、点、连字符、加号、字母（实际版本号就这些，如 `0.1.6`、`0.2.0-rc.1`），
/// 并且必须以字母或数字开头——挡掉 `..`、`/`、`\`、盘符与空串。
pub fn is_safe_version(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.len() > 64 {
        return false;
    }
    let first = v.chars().next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        // 单独一个点或纯点号组合（`.` / `..`）也不是版本号
        && v.chars().any(|c| c.is_ascii_alphanumeric())
}

/// 每个实例一份独立的 DSH 主目录：`<app_data>/instances/<实例 id>/home`。
///
/// 多个实例要真的互不干扰，profile 与插件就不能共用一份
/// （`<DSH_HOME>/profiles/<name>` 会被同时读写的插件管理命令打架）。
pub fn instance_home_dir(id: &str) -> PathBuf {
    instance_dir(id).join("home")
}

/// 把实例 id 收敛成安全的目录名。
pub fn safe_slug(s: &str) -> String {
    let out: String = s
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// 磁盘上已经存在的实例 id 集合（`homes/` 下已有的目录名）。
///
/// 新建实例时要拿它来避开"已被删除、但 DSH_HOME 还留着"的 id：
/// 删实例不删数据，所以 `inst2` 这个名字在实例消失后**仍然**指向一个装满
/// profile 与凭据的目录。新实例要是复用了同一个 id，就会默默接管上一个实例的数据。
///
/// 读取失败（目录不存在 / 没权限）时返回空集合——那就不避开任何 id，
/// 退化成"只看当前实例列表"的老行为，不会比原来更差。
pub fn existing_instance_ids() -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(app_data_dir().join("homes")) {
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(n) = e.file_name().to_str() {
                    out.insert(n.to_string());
                }
            }
        }
    }
    out
}

/// 某个实例的服务日志（token 也从这里解析）。
///
/// 多实例必须各写一份：`latest_token` 是"从某个偏移之后找 token"，
/// 几个实例往同一个文件里写就会互相抓到对方的 token。
pub fn instance_server_log(id: &str) -> PathBuf {
    data_dir().join(format!("DSH-Launch-Console-server-{}.log", safe_slug(id)))
}

/// 启动器日志。
pub fn launcher_log() -> PathBuf {
    std::env::temp_dir().join("DSH-Launch-Console.log")
}

/// DSH 服务输出日志（第一个实例用的历史路径；其余实例见 [`instance_server_log`]）。
pub fn server_log() -> PathBuf {
    std::env::temp_dir().join("DSH-Launch-Console-server.log")
}

/// 设置文件（JSON）。放临时目录，不新建目录。
pub fn settings_file() -> PathBuf {
    data_dir().join("DSH-Launch-Console-settings.json")
}

/// DSH 用户主目录（profile / 插件都在这里）。
pub fn dsh_home() -> PathBuf {
    if let Some(v) = env_nonempty("DSH_HOME") {
        return PathBuf::from(v);
    }
    let home = home_dir();
    home.join(".dsh")
}

/// 当前用户主目录。
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }
}

pub fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// DSH Web UI 地址（可在设置里改端口）。
pub const DEFAULT_URL: &str = "http://127.0.0.1:3080";

/// npm 包名（版本检索与安装都用它；与下面的源码仓库是同一来源）。
pub const DSH_PACKAGE: &str = "@deepseek-ai/dsh";

/// DeepSeek Harness 的源码仓库（界面上「源码仓库」按钮用）。
pub const DSH_REPO: &str = "https://github.com/deepseek-ai/deepseek-harness";

/// DSH 的 profile 名（插件装入哪个 profile）。
pub const DEFAULT_PROFILE: &str = "web";

/// 从 DSH 地址解出 host / port。
pub fn url_host_port(url: &str) -> (String, u16) {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((h, p)) => (h.trim().to_string(), p.trim().parse::<u16>().unwrap_or(3080)),
        None => (authority.trim().to_string(), 3080),
    }
}

/// 统一的 HTTP 客户端：**必须带超时**。
///
/// 插件市场那个接口有 4 MB 上下、实测要十几秒，而网络一旦卡住，没有超时的
/// 请求会让后台线程永远挂着——界面上就是一直转圈、既不报错也不能重试。
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .user_agent("DSH-Launch-Console")
            // 连不上就别耗着
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            // 含读取响应体的总时限：给足市场那 4 MB 的余量
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .build(),
    )
}

/// 追加一行到启动器日志（失败静默——日志永远不该拖垮 UI）。
pub fn log(msg: &str) {
    use std::io::Write;
    let path = launcher_log();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", secs, msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_version_accepts_real_versions_and_rejects_traversal() {
        for ok in ["0.1.6", "0.2.0", "0.2.0-rc.1", "1.0.0+build.5", "0.1.7-rc.1"] {
            assert!(is_safe_version(ok), "{} 应当被接受", ok);
        }
        // 这些都是要拼进路径的，必须挡掉
        for bad in ["", "   ", "..", ".", "../x", "a/b", "a\\b", "C:", "-1.0", ".hidden", "/"] {
            assert!(!is_safe_version(bad), "{:?} 应当被拒绝", bad);
        }
        // 超长的也拒掉，避免撑出非法路径
        assert!(!is_safe_version(&"1".repeat(65)));
    }

    #[test]
    fn safe_slug_flattens_unsafe_ids() {
        assert_eq!(safe_slug("default"), "default");
        assert_eq!(safe_slug("web-2"), "web-2");
        assert_eq!(safe_slug("a b/c"), "a-b-c");
        assert_eq!(safe_slug("../../etc"), "------etc");
        assert_eq!(safe_slug(""), "default");
        assert_eq!(safe_slug("  "), "default");
    }

    #[test]
    fn instance_version_dir_stays_under_that_instance() {
        // 版本目录必须落在**这个实例自己的**目录下——换个实例就换个根，
        // 这是"删实例能把它装的版本一起删干净"的前提。
        let a = instance_version_dir("jia", "0.1.6");
        let b = instance_version_dir("yi", "0.1.6");
        assert!(a.starts_with(instance_dir("jia")));
        assert!(b.starts_with(instance_dir("yi")));
        assert_ne!(a, b, "两个实例的同名版本不能是同一个目录");
        assert!(a.starts_with(instance_versions_dir("jia")));

        // 即使用户输入了带斜杠的东西，收敛后也还在这个实例的目录下面
        let evil = instance_versions_dir("jia").join(safe_slug("../evil"));
        assert!(evil.starts_with(instance_dir("jia")));
    }

    #[test]
    fn homes_are_per_instance_and_inside_their_own_dir() {
        assert!(instance_home_dir("jia").starts_with(instance_dir("jia")));
        assert_ne!(instance_home_dir("jia"), instance_home_dir("yi"));
    }
}

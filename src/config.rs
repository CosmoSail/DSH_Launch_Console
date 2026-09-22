//! 路径与运行期配置。
//!
//! **不新建任何目录**：设置与版本列表缓存这两个小文件直接放进系统临时目录
//! （启动器日志本来也在那儿）。0.2.0 起不再有「自管版本目录」，
//! 所以 `%LOCALAPPDATA%\DSH-Launch-Console` 不会再被创建，也不会再回来。

use std::path::PathBuf;

/// 数据目录：直接用系统临时目录，**不创建任何目录**。
pub fn data_dir() -> PathBuf {
    std::env::temp_dir()
}

/// 启动器日志。
pub fn launcher_log() -> PathBuf {
    std::env::temp_dir().join("DSH-Launch-Console.log")
}

/// DSH 服务输出日志（token 也在这里打印）。
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

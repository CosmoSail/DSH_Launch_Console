//! 持久化设置。

use serde::{Deserialize, Serialize};

use crate::config;

/// ✕ 按钮行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CloseAction {
    /// 最小化到系统托盘继续运行
    Tray,
    /// 直接退出（并关闭 DSH）
    Exit,
}

impl Default for CloseAction {
    fn default() -> Self {
        Self::Tray
    }
}

/// 界面风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// 浅色（明亮）
    Light,
    /// 深色（高对比：不用灰字）
    Dark,
}

impl Default for ThemeMode {
    fn default() -> Self {
        Self::Light
    }
}

/// 启动器设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// DSH Web UI 地址
    pub url: String,
    /// 使用的 profile
    pub profile: String,
    /// ✕ 按钮行为
    pub close_action: CloseAction,
    /// 启动 DSH 后自动打开浏览器
    pub auto_open_browser: bool,
    /// 界面风格。加 default 是为了老设置文件（没有这个键）也能正常读出来
    #[serde(default)]
    pub theme: ThemeMode,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: config::DEFAULT_URL.to_string(),
            profile: config::DEFAULT_PROFILE.to_string(),
            close_action: CloseAction::Tray,
            auto_open_browser: true,
            theme: ThemeMode::Light,
        }
    }
}

impl Settings {
    /// 读取设置。优先级：环境变量 > settings.json > 默认值。
    ///
    /// `DSH_LAUNCH_CONSOLE_URL` 连界面一起覆盖（不只是自检）：这样可以用
    /// 独立互斥体 + 独立端口并存一个测试实例，绝不碰正在用的会话。
    pub fn load() -> Self {
        let path = config::settings_file();
        let mut s = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .unwrap_or_default();
        if let Some(url) = config::env_nonempty("DSH_LAUNCH_CONSOLE_URL") {
            s.url = url;
        }
        s
    }

    /// 保存设置。不新建目录（数据目录恒存在），写失败就算了——
    /// 设置丢了只影响下次启动的默认值，不该拖垮界面。
    pub fn save(&self) {
        if let Ok(txt) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config::settings_file(), txt);
        }
    }

    pub fn host_port(&self) -> (String, u16) {
        config::url_host_port(&self.url)
    }
}

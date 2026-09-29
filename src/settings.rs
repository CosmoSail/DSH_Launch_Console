//! 持久化设置。

use serde::{Deserialize, Serialize};

use crate::config;
use crate::i18n::Lang;
use crate::tr;

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

/// 全局实例的固定 id。
///
/// 侧栏里那一条**全局实例**是不可删除、也不可改版本的：它就是"用全局安装的那份
/// DSH、走默认安装目录（`~/.dsh`）"。所有其它实例都是它的对照——各自装各自的版本、
/// 各有各的 DSH_HOME，与全局、以及彼此之间完全隔离。
pub const GLOBAL_ID: &str = "global";

/// 一个 DSH 实例的配置（不含运行时状态——那是 app 里的事）。
///
/// 多实例的核心就是这份结构：每个实例有自己的端口、profile 与 DSH 主目录，
/// 因此可以同时跑多个互不干扰的 DSH；`version` 为空表示用**全局安装**那一份，
/// 填了版本号就用启动器自管目录里装的那一份。
///
/// 唯一的例外是 [`GLOBAL_ID`] 那个全局实例：它被固定成
/// `version = ""` + `own_home = false` + `home = ""`，也就是
/// "全局安装 + 默认 DSH_HOME"。这不是"随便一个碰巧这么配的实例"，
/// 所以由 [`InstanceConfig::is_global`] 判定，并且每次 [`Settings::normalize_global`]
/// 都会把这几个字段掰回去——否则手工编辑设置文件就能做出一个"名字叫全局、
/// 实际跑别的版本"的实例，把"全局"这个词的含义搞乱。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// 稳定 id：用来定 DSH_HOME 与日志文件名，创建后不再变
    pub id: String,
    /// 界面上显示的名字（可改；全局实例固定为「全局实例」）
    pub name: String,
    /// DSH 版本；空串 = 用全局安装那一份
    #[serde(default)]
    pub version: String,
    /// 监听端口
    pub port: u16,
    /// 这个实例用哪个 profile
    #[serde(default)]
    pub profile: String,
    /// 是否给这个实例单独一份 DSH_HOME
    ///
    /// 默认开：多个实例共用一份 profile 会被插件管理命令同时读写，互相打架。
    /// 关掉则回落到全局 `~/.dsh`（想共用同一套插件时才关）。
    /// 全局实例固定为 `false`——它用的就是默认安装目录。
    #[serde(default = "default_true")]
    pub own_home: bool,
    /// 自定义 DSH_HOME（留空则按 `own_home` 自动决定）
    #[serde(default)]
    pub home: String,
}

fn default_true() -> bool {
    true
}

impl InstanceConfig {
    /// 是不是那个固定的全局实例。
    pub fn is_global(&self) -> bool {
        self.id == GLOBAL_ID
    }

    /// 这个实例实际用的 DSH_HOME。
    ///
    /// 优先级：显式填的 `home` > 独立 home（按 id 派生） > 全局 `~/.dsh`。
    pub fn resolved_home(&self) -> std::path::PathBuf {
        let custom = self.home.trim();
        if !custom.is_empty() {
            return std::path::PathBuf::from(custom);
        }
        if self.own_home {
            config::instance_home_dir(&self.id)
        } else {
            config::dsh_home()
        }
    }

    /// 这个实例的服务日志（token 也从这里解析）。
    pub fn log_path(&self) -> std::path::PathBuf {
        config::instance_server_log(&self.id)
    }

    /// 监听地址（跟着设置里的 host 走，只换端口）。
    pub fn url(&self, host: &str) -> String {
        format!("http://{}:{}", host, self.port)
    }

    pub fn profile_or_default(&self) -> String {
        let p = self.profile.trim();
        if p.is_empty() {
            config::DEFAULT_PROFILE.to_string()
        } else {
            p.to_string()
        }
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
    /// 检查到新版本时**自动更新已装插件**。
    ///
    /// - `false`（默认）：只自动检索最新版本，在插件页标出来，点「更新」并确认后才装
    /// - `true`：检索到新版本直接装（每次运行最多自动跑一轮）
    ///
    /// 默认关：插件是用户自己挑的，静默升级不该是默认行为。加 default 同样是为了
    /// 老设置文件能读出来。
    #[serde(default)]
    pub auto_update_plugins: bool,
    /// 界面语言（中文 / English）。
    ///
    /// 老设置文件没有这个键时按中文读（`Lang::default()`）；**全新安装**则跟系统
    /// 区域走，见 `load()`。
    #[serde(default)]
    pub language: Lang,
    /// 实例列表（0.3.1 起）。空数组表示还没建过实例，`load()` 会按老设置补一个。
    #[serde(default)]
    pub instances: Vec<InstanceConfig>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            url: config::DEFAULT_URL.to_string(),
            profile: config::DEFAULT_PROFILE.to_string(),
            close_action: CloseAction::Tray,
            auto_open_browser: true,
            theme: ThemeMode::Light,
            auto_update_plugins: false,
            language: Lang::default(),
            instances: Vec::new(),
        }
    }
}

/// 递归拷贝一棵目录树（跨盘符搬不了时用它兜底）。
fn copy_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let from = e.path();
        let to = dst.join(e.file_name());
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

impl Settings {
    /// 读取设置。优先级：环境变量 > settings.json > 默认值。
    ///
    /// `DSH_LAUNCH_CONSOLE_URL` 连界面一起覆盖（不只是自检）：这样可以用
    /// 独立互斥体 + 独立端口并存一个测试实例，绝不碰正在用的会话。
    pub fn load() -> Self {
        let path = config::settings_file();
        let mut s = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        {
            Some(s) => s,
            // 全新安装（还没有设置文件）：界面语言先跟系统区域走，
            // 之后一律以下拉框里选的那份为准
            None => Self { language: Lang::from_system(), ..Default::default() },
        };
        if let Some(url) = config::env_nonempty("DSH_LAUNCH_CONSOLE_URL") {
            s.url = url;
        }
        s.migrate_instances();
        s.normalize_global();
        // 存储布局迁移放在最后：它要按最终确定下来的实例列表去搬文件
        s.migrate_storage_layout();
        s
    }

    /// 把旧版（0.3.1 开发期）的"共用版本库 + 独立 homes 根"搬进各实例自己的目录。
    ///
    /// 旧布局：`<data>/versions/<版本>`（所有实例共用一份）、`<data>/homes/<id>`（各自的 home）。
    /// 新布局：`<data>/instances/<id>/{home, versions/<版本>}`（一个实例一个目录，
    /// 删实例时能一次删干净）。
    ///
    /// 做法：哪个实例的配置指着某个共用版本，就把那份**搬**给它；有两个实例指着同一个版本时，
    /// 第一个搬、后面的拷贝一份（因为新布局下不共用，必须各有一份）。
    /// 幂等：目标已存在就跳过；全部搬完且共用库里没剩东西时，把老的共用目录删掉。
    ///
    /// 这里刻意只做"能安全做对"的部分：搬不动（占用、权限）就保留原样并记日志，
    /// 宁可让那个实例暂时回到"没装这个版本"（界面上会提示重新装），也不要半搬半留把数据搞坏。
    pub fn migrate_storage_layout(&mut self) {
        let legacy_versions = config::legacy_versions_dir();
        if legacy_versions.is_dir() {
            // 收集"谁要用哪个版本"：按版本号分组，保持实例顺序（第一个搬、其余拷贝）
            let mut wanted: Vec<(String, String)> = Vec::new(); // (实例 id, 版本)
            for inst in &self.instances {
                if inst.is_global() {
                    continue; // 全局实例用的是全局安装那一份，与共用库无关
                }
                let v = inst.version.trim();
                if !v.is_empty() && config::is_safe_version(v) {
                    wanted.push((inst.id.clone(), v.to_string()));
                }
            }
            let mut moved_any: Vec<String> = Vec::new();
            for (id, v) in &wanted {
                let src = legacy_versions.join(v);
                let dst = config::instance_version_dir(id, v);
                if !src.is_dir() || dst.exists() {
                    continue;
                }
                if std::fs::rename(&src, &dst).is_ok() {
                    moved_any.push(v.clone());
                } else if copy_tree(&src, &dst).is_ok() {
                    moved_any.push(v.clone());
                } else {
                    config::log(&format!(
                        "storage migration: 无法把共用版本 {} 搬给实例 {}，保留原样",
                        v, id
                    ));
                }
            }
            // 共用库里已经空了（所有被引用的版本都搬走/拷走了）→ 删掉老目录
            let still_referenced: Vec<&String> = wanted
                .iter()
                .map(|(_, v)| v)
                .filter(|v| legacy_versions.join(v).is_dir())
                .collect();
            if still_referenced.is_empty() {
                let _ = std::fs::remove_dir_all(&legacy_versions);
            }
            if !moved_any.is_empty() {
                config::log(&format!(
                    "storage migration: 把共用版本搬进实例目录 {:?}",
                    moved_any
                ));
            }
        }

        // 老的 `homes/<id>` → `instances/<id>/home`
        for inst in &self.instances {
            let old = config::legacy_home_dir(&inst.id);
            let new = config::instance_home_dir(&inst.id);
            if old.is_dir() && !new.exists() {
                if let Some(parent) = new.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::rename(&old, &new).is_err() && copy_tree(&old, &new).is_err() {
                    config::log(&format!(
                        "storage migration: 无法把 {} 搬到 {}，保留原样",
                        old.display(),
                        new.display()
                    ));
                }
            }
        }
        let legacy_homes = config::app_data_dir().join("homes");
        if legacy_homes.is_dir() {
            // 空了才删（里面还有东西说明有实例没搬成功）
            let empty = std::fs::read_dir(&legacy_homes)
                .map(|rd| rd.flatten().next().is_none())
                .unwrap_or(false);
            if empty {
                let _ = std::fs::remove_dir_all(&legacy_homes);
            }
        }
    }

    /// 全局实例的出厂配置。
    pub fn global_instance(port: u16, profile: &str) -> InstanceConfig {
        InstanceConfig {
            id: GLOBAL_ID.to_string(),
            name: tr!("全局实例").to_string(),
            // 空版本 = 用全局安装那一份；这正是"全局实例"的定义
            version: String::new(),
            port,
            profile: profile.to_string(),
            // 用默认安装目录 ~/.dsh，不另开一份
            own_home: false,
            home: String::new(),
        }
    }

    /// 老设置文件（0.2.5 及以前）没有 `instances`，而那时 `url` / `profile`
    /// 就是"当前那个实例"的全部描述。这里把它补成**全局实例**，
    /// 让升级后行为与升级前**完全一致**：同一个端口、同一个 profile、同一份全局安装。
    pub fn migrate_instances(&mut self) {
        // 0.3.1 之前这条叫 "default"；统一改名为 "global"，
        // 免得同一个东西在设置文件里有两个名字（旧设置文件升级上来会遇到）。
        for i in &mut self.instances {
            if i.id == "default" {
                i.id = GLOBAL_ID.to_string();
            }
        }
        // 万一新旧两个 id 同时存在（手工编辑过），只留一个
        let mut seen_global = false;
        self.instances.retain(|i| {
            if i.is_global() {
                if seen_global {
                    return false;
                }
                seen_global = true;
            }
            true
        });

        if self.instances.iter().any(|i| i.is_global()) {
            return;
        }
        // 有别的实例但没全局实例 → 把全局实例插到最前面（它永远排第一）
        let (_, port) = self.host_port();
        let g = Self::global_instance(port, &self.profile);
        self.instances.insert(0, g);
    }

    /// 把全局实例的那几个"定义性字段"掰回固定值。
    ///
    /// 每次加载都跑一遍：设置文件是可以手工编辑的，而这个实例的语义
    /// （全局安装 + 默认 DSH_HOME）必须唯一，否则"全局"就名不副实了。
    /// 名字也一并固定——它不是一个可随意改名的普通实例。
    pub fn normalize_global(&mut self) {
        let fixed_name = tr!("全局实例").to_string();
        for i in &mut self.instances {
            if !i.is_global() {
                continue;
            }
            i.version.clear();
            i.own_home = false;
            i.home.clear();
            if i.name != fixed_name {
                i.name = fixed_name.clone();
            }
        }
    }

    /// 找一个当前没被占用的端口：从 `from` 起往上试。
    ///
    /// 只看自己的实例列表——别的程序占了哪个端口这里查不到，
    /// 真正的冲突由启动失败时的报错兜住。
    pub fn next_free_port(&self, from: u16) -> u16 {
        let used: Vec<u16> = self.instances.iter().map(|i| i.port).collect();
        let mut p = from.max(1024);
        while used.contains(&p) && p < 65535 {
            p += 1;
        }
        p
    }

    /// 新的实例用哪个端口：从「已有端口的最大值 + 1」开始往上找。
    ///
    /// 不能从默认 3080 往上试——默认实例就占着 3080，那样新建的实例会
    /// 一路撞到它的端口上去（曾经写成 `host_port().1 + 1`，而 host_port 读的是
    /// `url` 字段，跟实例列表无关，于是新实例会和默认实例抢同一个端口）。
    pub fn suggest_port(&self) -> u16 {
        let base = self
            .instances
            .iter()
            .map(|i| i.port)
            .max()
            .unwrap_or_else(|| self.host_port().1);
        self.next_free_port(base.saturating_add(1))
    }

    /// 新建实例建议用哪个版本。
    ///
    /// **自建实例只用自己目录里的版本**（全局那一份是全局实例的），而新实例还什么都没有，
    /// 所以这里只能给一个"建议装哪个"：版本列表缓存里的 `latest`。
    /// 拿到之后如果不等于全局安装的版本，启动时会先去装（见 `App::start_instance`）。
    /// 缓存还没有（刚装好、还没查过版本列表）时返回空串，界面上会提示先去装一个。
    pub fn recommended_version(&self) -> String {
        crate::versions::load_cache()
            .map(|c| c.latest)
            .filter(|l| !l.trim().is_empty())
            .unwrap_or_default()
    }

    /// 新建一个实例配置（端口自动挑一个没被自己占用的）。
    ///
    /// `version` 由调用方给（通常传 [`Settings::recommended_version`]）：
    /// **自建实例只用自管版本**，所以新建时就该带一个版本号。做成参数而不是在这里
    /// 自己探测，是为了让这个函数保持**纯**——它会读目录、读版本缓存，
    /// 放在函数里会让单元测试的结果随机器状态变化。
    ///
    /// `id_free` 用来判断某个候选 id 还能不能用。除了"没有实例正占着这个 id"，
    /// 调用方还必须把**磁盘上已存在的 DSH_HOME 目录**算进去，原因见
    /// [`crate::config::instance_home_dir`]：删实例**不会**删它的 DSH_HOME，
    /// 所以 `inst2` 这个 id 在实例删掉之后仍然对应着一个装满数据的目录。
    /// 新实例若复用了同一个 id，就会悄无声息地接管上一个实例的 profile 与登录凭据。
    pub fn new_instance(
        &self,
        name: String,
        version: String,
        id_free: impl Fn(&str) -> bool,
    ) -> InstanceConfig {
        // id 要与已有实例、以及磁盘上遗留的 home 目录都区分开
        let mut n = self.instances.len() + 1;
        while !id_free(&format!("inst{}", n)) {
            n += 1;
        }
        InstanceConfig {
            id: format!("inst{}", n),
            name,
            version,
            port: self.suggest_port(),
            profile: self.profile.clone(),
            own_home: true,
            home: String::new(),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_migrate_to_a_single_equivalent_instance() {
        // 模拟 0.2.5 的设置文件：没有 instances 字段
        let old = r#"{
          "url": "http://127.0.0.1:3199",
          "profile": "tui",
          "close_action": "tray",
          "auto_open_browser": false
        }"#;
        let mut s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.instances.is_empty());
        s.migrate_instances();
        assert_eq!(s.instances.len(), 1);
        let i = &s.instances[0];
        assert_eq!(i.port, 3199, "端口要跟着老 url 走");
        assert_eq!(i.profile, "tui", "profile 要跟着老设置走");
        assert_eq!(i.version, "", "默认用全局安装那一份");
        assert!(!i.own_home, "老配置只有一份 ~/.dsh，迁移后不该突然换 home");
        assert_eq!(i.resolved_home(), config::dsh_home());
        // 迁移出来的那条就是全局实例（0.3.1 之前叫 default）
        assert!(i.is_global());
        assert_eq!(i.id, GLOBAL_ID);
    }

    /// 全局实例是"用全局安装那份 DSH"的唯一代表，必须永远存在，且排在第一位。
    #[test]
    fn global_instance_always_exists_first() {
        // 全新设置
        let mut s = Settings::default();
        s.migrate_instances();
        assert_eq!(s.instances.len(), 1);
        assert!(s.instances[0].is_global(), "第一条必须是全局实例");
        assert_eq!(s.instances[0].version, "");
        assert!(!s.instances[0].own_home);
        assert_eq!(s.instances[0].resolved_home(), config::dsh_home());

        // 已有别的实例、但缺全局实例 → 补到最前面
        let mut s2 = Settings::default();
        s2.instances = vec![InstanceConfig {
            id: "inst2".into(),
            name: "x".into(),
            version: "0.1.7-rc.1".into(),
            port: 3081,
            profile: "web".into(),
            own_home: true,
            home: String::new(),
        }];
        s2.migrate_instances();
        assert_eq!(s2.instances.len(), 2);
        assert!(s2.instances[0].is_global(), "全局实例要补在最前面");
        assert_eq!(s2.instances[1].id, "inst2", "原有实例不能被挪走或改掉");
    }

    /// 设置文件是可以手工编辑的：全局实例的那几个定义性字段每次加载都要被掰回来，
    /// 否则"名字叫全局、实际跑别的版本"会让"全局"这个词失去意义。
    #[test]
    fn normalize_global_reinstates_its_fixed_fields() {
        let mut s = Settings::default();
        s.instances = vec![InstanceConfig {
            id: GLOBAL_ID.into(),
            name: "我改的名字".into(),
            version: "0.1.6-alpha.2".into(),
            port: 3080,
            profile: "web".into(),
            own_home: true,
            home: r"D:\somewhere-else".into(),
        }];
        s.normalize_global();
        let g = &s.instances[0];
        assert_eq!(g.version, "", "全局实例必须用全局安装那一份");
        assert!(!g.own_home, "全局实例必须用默认 DSH_HOME");
        assert_eq!(g.resolved_home(), config::dsh_home());
        assert_ne!(g.name, "我改的名字", "名字是固定的");
        assert_eq!(g.port, 3080, "端口是实例自己的事，不动它");
    }

    /// 归一化不能误伤普通实例
    #[test]
    fn normalize_global_leaves_other_instances_alone() {
        let mut s = Settings::default();
        s.instances = vec![
            Settings::global_instance(3080, "web"),
            InstanceConfig {
                id: "inst2".into(),
                name: "备用".into(),
                version: "0.1.7-rc.1".into(),
                port: 3081,
                profile: "tui".into(),
                own_home: true,
                home: String::new(),
            },
        ];
        s.normalize_global();
        let o = &s.instances[1];
        assert_eq!(o.version, "0.1.7-rc.1", "普通实例的版本不能被清掉");
        assert!(o.own_home);
        assert_eq!(o.name, "备用");
        assert_eq!(o.profile, "tui");
    }

    /// 0.3.1 之前全局那条的 id 是 "default"：升级上来要改名，且不能变成两条。
    #[test]
    fn legacy_default_id_is_renamed_to_global() {
        let mut s = Settings::default();
        s.instances = vec![InstanceConfig {
            id: "default".into(),
            name: "默认实例".into(),
            version: String::new(),
            port: 3080,
            profile: "web".into(),
            own_home: false,
            home: String::new(),
        }];
        s.migrate_instances();
        assert_eq!(s.instances.len(), 1, "不能变成两条");
        assert_eq!(s.instances[0].id, GLOBAL_ID);
        assert_eq!(s.instances[0].port, 3080, "端口要保住");
    }

    #[test]
    fn migrate_is_idempotent() {
        let mut s = Settings::default();
        s.migrate_instances();
        let first = s.instances.clone();
        s.migrate_instances();
        assert_eq!(s.instances.len(), 1);
        assert_eq!(s.instances[0].id, first[0].id);
    }

    #[test]
    fn new_instances_get_distinct_ports() {
        let mut s = Settings::default();
        s.url = "http://127.0.0.1:3080".into();
        s.migrate_instances();
        // 全局实例占用 3080（跟着老 url 走），这是对的
        assert_eq!(s.instances[0].port, 3080);
        assert!(s.instances[0].is_global());
        for n in ["a", "b", "c"] {
            let i = s.new_instance(n.to_string(), "0.1.7-rc.2".into(), |_| true);
            s.instances.push(i);
        }
        let ports: Vec<u16> = s.instances.iter().map(|i| i.port).collect();
        let mut uniq = ports.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(ports.len(), uniq.len(), "端口不能重复: {:?}", ports);
        // 新建的实例必须避开已有端口，尤其不能撞上全局实例那个
        let new_ports: Vec<u16> = ports[1..].to_vec();
        assert!(
            !new_ports.contains(&3080),
            "新实例不该抢全局实例的 3080: {:?}",
            new_ports
        );
        assert_eq!(ports, vec![3080, 3081, 3082, 3083]);
    }

    /// 删实例**不删**它的 DSH_HOME，所以新实例绝不能复用被删掉的 id——
    /// 否则新实例会直接接管上一个实例的 profile 与登录凭据。
    #[test]
    fn new_instance_skips_ids_with_leftover_homes() {
        let mut s = Settings::default();
        s.url = "http://127.0.0.1:3080".into();
        s.migrate_instances(); // 得到 default
        s.instances.push(s.clone().new_instance("b".into(), "0.1.7-rc.2".into(), |_| true)); // 得到 inst2

        // 现在模拟：inst2 被删了，但 homes/inst2 还在磁盘上。
        // id 生成必须跳过 inst2，落到 inst3。
        let leftover = ["inst2".to_string()];
        let cfg = s.new_instance("c".into(), "0.1.7-rc.2".into(), |id| {
            !leftover.contains(&id.to_string())
        });
        assert_eq!(cfg.id, "inst3", "不能复用已被删除、但 home 目录还留着的 inst2");
    }

    /// 反向确认：没有遗留目录时仍然从 inst2 开始（不要因为修 bug 把编号整体挪走）
    #[test]
    fn new_instance_uses_the_lowest_free_id() {
        let mut s = Settings::default();
        s.url = "http://127.0.0.1:3080".into();
        s.migrate_instances();
        let cfg = s.new_instance("b".into(), "0.1.7-rc.2".into(), |_| true);
        assert_eq!(cfg.id, "inst2");
    }

    #[test]
    fn instance_home_precedence() {
        let mut i = InstanceConfig {
            id: "abc".into(),
            name: "x".into(),
            version: String::new(),
            port: 3081,
            profile: "web".into(),
            own_home: true,
            home: String::new(),
        };
        // 独立 home：按 id 派生，且**必须落在该实例自己的目录下**——
        // 这样删实例时一次目录删除就能连 home 一起清掉
        assert_eq!(i.resolved_home(), config::instance_home_dir("abc"));
        assert!(i.resolved_home().starts_with(config::instance_dir("abc")));
        // 显式指定优先
        i.home = "D:\\myhome".into();
        assert_eq!(i.resolved_home(), std::path::PathBuf::from("D:\\myhome"));
        // 关掉独立 home → 回落全局
        i.home = String::new();
        i.own_home = false;
        assert_eq!(i.resolved_home(), config::dsh_home());
    }

    #[test]
    fn url_built_from_port() {
        let i = InstanceConfig {
            id: "x".into(),
            name: "x".into(),
            version: String::new(),
            port: 3199,
            profile: String::new(),
            own_home: false,
            home: String::new(),
        };
        assert_eq!(i.url("127.0.0.1"), "http://127.0.0.1:3199");
        assert_eq!(i.profile_or_default(), config::DEFAULT_PROFILE);
    }
}

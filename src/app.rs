//! 启动器主界面。
//!
//! 布局：顶栏（图标 + 名称 + 全局状态）+ 左侧导航（控制台 / 版本 / 插件 / 设置）
//! + 中央内容区。所有耗时操作（npm 检索、插件市场、启动等待）都放后台线程，
//! 结果经 channel 回主线程，UI 永不阻塞。

use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::{tr, trf};

use crate::config;
use crate::dsh;
use crate::i18n::{self, Lang};
use crate::icon;
use crate::instances;
use crate::plugins::{self, InstalledPlugin, PluginInfo, PluginOrigin, PluginRoute};
use crate::settings::{self, CloseAction, Settings, ThemeMode};
use crate::versions::{self, Installed, VersionList};
use crate::webui;
use crate::tray::{self, TrayAction};

/// 实例启动超时：DSH 要挂载整棵 cordis 插件树，机器忙时明显变慢
/// （`tests/test-startup-timing.ps1` 实测波动 2.3s ~ 28.4s，给了很大余量）。
const INSTANCE_START_TIMEOUT_SECS: u64 = 180;

// ------------------------------------------------------------------ 阶段

/// 一个实例的运行状态：配置 + 本进程观察到的情况。
///
/// 真正的实例逻辑在 [`crate::instances`]，这里只是把它连同一份界面状态一起放在
/// `App` 里。界面状态目前用不到额外字段（选中/展开都由 `App` 的 `selected` /
/// `section` 决定），但保留这层包装是为了将来加"每实例的界面状态"时不用改结构。
struct InstItem {
    inst: instances::Instance,
}

impl InstItem {
    fn new(cfg: settings::InstanceConfig) -> Self {
        Self { inst: instances::Instance::new(cfg) }
    }
}

/// 控制台里当前看哪一段。
///
/// 实例的控制台把「启停 / 实例配置 / 版本管理 / 插件管理」收在一处，
/// 用分段切换而不是拆成四个页面：这些都是**同一个实例**的属性，
/// 拆开就得反复在侧栏里跳。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// 状态 + 启停 + 实例配置（一个实例一眼看完）
    Overview,
    /// 版本管理：装哪个 DSH 版本、全局安装与自管版本
    Versions,
    /// 插件管理：市场 / GitHub / 已装列表
    Plugins,
}

/// 右侧主区显示哪个页面。
///
/// 侧栏结构：**启动器设置**（整个程序的行为）+ **实例列表**（每个实例一台控制台）。
/// 所以只有两类页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// 启动器自己的设置：语言、风格、关闭行为、插件更新策略
    Launcher,
    /// 某个实例的控制台（看的是 `selected` 那个实例）
    Console,
}

// ------------------------------------------------------------------ 后台任务

/// 后台任务结果。
enum TaskResult {
    Versions(Result<VersionList, String>),
    Market(Result<Vec<PluginInfo>, String>),
    Github(Result<Vec<PluginInfo>, String>),
    /// 已装插件的最新版本检查：(包名 → latest, 提示)
    PluginLatest(BTreeMap<String, String>, Vec<String>),
    /// 把某个版本装到自管目录的结果：(版本, 结果)
    VersionReady(String, Result<String, String>),
    /// 通用操作结果：(标题, 结果)
    Op(String, Result<String, String>),
}

/// 后台任务句柄。
struct Tasks {
    tx: Sender<TaskResult>,
    rx: Receiver<TaskResult>,
    /// 进行中的任务数（>0 时界面持续重绘）
    inflight: usize,
}

impl Tasks {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, inflight: 0 }
    }

    /// 起一个后台线程。
    fn spawn<F>(&mut self, ctx: &egui::Context, f: F)
    where
        F: FnOnce() -> TaskResult + Send + 'static,
    {
        self.inflight += 1;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = f();
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }
}

// ------------------------------------------------------------------ App

pub struct App {
    /// 右侧主区显示哪一类页面
    page: Page,
    /// 实例控制台里当前看哪一段
    section: Section,
    settings: Settings,

    /// 后台任务
    tasks: Tasks,

    /// 所有 DSH 实例。**这就是多实例的本体**：每个实例有自己的端口、profile、
    /// DSH_HOME 与版本，各自一个进程句柄，可以同时跑。
    insts: Vec<InstItem>,
    /// 当前选中的实例（插件页 / 版本页按它来扫描与安装）
    selected: usize,
    /// 自管版本目录里已经装了哪些版本（启动时读一次，安装后刷新）
    managed_versions: Vec<String>,
    /// 有多少个实例还在启动中（决定要不要持续重绘）
    starting_count: usize,
    /// 正在装到自管目录的版本（与 per-instance 的 installing 联动：
    /// 概览和版本页是两个入口，两边都得显示成"正在装"）
    installing_managed: Option<String>,
    /// 实例行上待执行的动作：(实例下标, 动作)。
    ///
    /// 行是画在 `show` 的闭包里的，那里已经可变借用了 `self`，
    /// 所以动作先存下来，等绘制结束（借用释放）再执行。
    pending_inst_act: Option<(usize, InstanceAct)>,

    /// 待删除的实例 id（确认条点了「删除」之后记下来）。
    ///
    /// 删除会**改变列表长度**，所以绝不能在本帧绘制中途做——同帧后面记下的下标会悬空。
    /// 记成 id（不是下标）还有一个好处：等真正执行时列表可能已经变了，按 id 找不会删错人。
    pending_remove: Option<String>,

    /// 待卸载的版本：`None` = 卸载全局安装；`Some((实例 id, 版本))` = 卸载那个实例的那个版本。
    /// 和 `pending_remove` 同理，等绘制结束再动磁盘。
    pending_uninstall: Option<Option<(String, String)>>,

    /// 正在卸载全局安装（后台跑 npm，期间别重复触发）
    uninstalling_global: bool,

    /// 版本页
    versions: VersionList,
    versions_fetched: bool,
    installed: Installed,
    /// 正在安装的版本（显示进度）
    installing: Option<String>,

    /// 插件页
    market: Vec<PluginInfo>,
    market_loaded: bool,
    /// 市场正在后台重取（刷新按钮点了之后转圈用）
    market_loading: bool,
    market_query: String,
    market_category: String,
    github_results: Vec<PluginInfo>,
    github_loading: bool,
    /// 最近一次多途径扫描的结果（已装插件 / 平台层 / 途径汇总）
    plugin_scan: plugins::ScanReport,
    /// 已安装插件列表实测的每行高度（点）：用来把列表固定成"一屏 4 行"
    plugin_row_h: f32,
    /// 后台查到的最新版本（包名 → latest）；没有的表示查不到（GitHub 装的/私有包）
    plugin_updates: BTreeMap<String, String>,
    /// 正在后台检查最新版本
    update_checking: bool,
    /// 本次运行是否已经跑过"自动更新"（避免反复重装）
    auto_update_ran: bool,
    /// 等用户确认的动作（更新插件）
    pending_confirm: Option<ConfirmAction>,
    plugin_busy: Option<String>,

    /// 提示消息 (文本, 是否错误)
    toast: Option<(String, bool)>,
    /// 最近一次操作输出（折叠显示）
    last_output: Option<String>,

    /// 托盘是否可用
    tray_ok: bool,
    /// 窗口当前是否隐藏到托盘
    hidden_to_tray: bool,
    /// 请求退出
    quit: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // 设置要最先读：风格决定后面整套配色的取值，语言决定后面所有文案
        let settings = Settings::load();
        theme::set_dark(settings.theme == ThemeMode::Dark);
        i18n::set(settings.language);

        let font = icon::install_cjk_font(&cc.egui_ctx);
        apply_theme(&cc.egui_ctx);
        let font_note = match &font {
            Some(p) => trf!("中文字体: {}", p),
            None => tr!("未找到系统中文字体，中文可能显示为方块").to_string(),
        };
        config::log(&format!(
            "launcher started ({}); {}; {}",
            env!("CARGO_PKG_VERSION"),
            font_note,
            if theme::is_dark() { "dark" } else { "light" }
        ));

        let tray_ok = tray::install(&cc.egui_ctx);
        // 托盘菜单要给「当前生效的那一项」打勾，先把设置同步过去
        tray::set_close_to_tray(settings.close_action == CloseAction::Tray);

        // 把主窗口 HWND 交给托盘（托盘菜单要显示/隐藏窗口）
        #[cfg(windows)]
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    tray::set_main_hwnd(w.hwnd.get() as isize);
                }
            }
        }

        let mut app = Self {
            page: Page::Console,
            section: Section::Overview,
            settings,
            tasks: Tasks::new(),
            insts: Vec::new(),
            selected: 0,
            managed_versions: Vec::new(),
            starting_count: 0,
            installing_managed: None,
            pending_inst_act: None,
            pending_remove: None,
            pending_uninstall: None,
            uninstalling_global: false,
            versions: VersionList::default(),
            versions_fetched: false,
            installed: Installed::default(),
            installing: None,
            market: Vec::new(),
            market_loaded: false,
            market_loading: false,
            market_query: String::new(),
            market_category: "全部".to_string(),
            github_results: Vec::new(),
            github_loading: false,
            plugin_scan: plugins::ScanReport::default(),
            plugin_row_h: PLUGIN_ROW_H_FALLBACK,
            plugin_updates: BTreeMap::new(),
            update_checking: false,
            auto_update_ran: false,
            pending_confirm: None,
            plugin_busy: None,
            toast: None,
            last_output: None,
            tray_ok,
            hidden_to_tray: false,
            quit: false,
        };

        // 启动即用缓存秒开，再后台刷新
        if let Some(c) = versions::load_cache() {
            app.versions = c;
            app.versions_fetched = true;
        }
        app.installed = versions::installed();
        // 版本是按实例分开的，这里不预扫；进「版本」页时按选中实例现扫（见 console_versions）
        // 实例列表来自设置（0.2.5 及以前的配置在 load() 里已迁移）
        app.insts = app
            .settings
            .instances
            .iter()
            .cloned()
            .map(InstItem::new)
            .collect();
        if app.insts.is_empty() {
            // 正常走不到这儿（load() 一定会补出全局实例）。
            // 真到这里了（比如实例列表被手工清空），补的就是**全局实例**——
            // 它不可删除，所以任何时候都该有一条。
            let (host, port) = app.settings.host_port();
            let _ = host;
            let cfg = settings::Settings::global_instance(port, &app.settings.profile);
            app.insts.push(InstItem::new(cfg));
        }
        app.refresh_versions(&cc.egui_ctx);
        app.refresh_plugins(&cc.egui_ctx);
        // 附加模式：端口已经在监听（用户之前自己开的）就算运行中。
        // 逐个实例查——多实例下"DSH 是否在运行"是每个实例各自的问题。
        let host = app.settings.host_port().0;
        let attached = app.attach_existing(&host);
        if attached > 0 {
            config::log(&format!("{} DSH instance(s) already listening; attaching", attached));
        }
        // 把规范化后的实例列表落盘。老设置文件（0.2.5 形态）里没有 instances，
        // 迁移只发生在内存里——不写回的话文件一直是旧形态，用户看不到自己的实例
        // 配置长什么样，手工改也无从下手。这里写一次，之后只在真的有改动时再写。
        app.persist_instances();
        app
    }

    // ---------------------------------------------------------- 实例

    /// 选中下标（越界时收敛）。
    fn sel(&self) -> usize {
        self.selected.min(self.insts.len().saturating_sub(1))
    }

    fn sel_item(&self) -> Option<&InstItem> {
        self.insts.get(self.sel())
    }

    /// 选中实例的 DSH_HOME（插件页按它扫描）。
    fn selected_home(&self) -> std::path::PathBuf {
        match self.sel_item() {
            Some(i) => i.inst.home(),
            None => config::dsh_home(),
        }
    }

    /// 选中实例用的 DSH 版本（空 = 全局安装那一份）。
    fn selected_version(&self) -> Option<String> {
        self.sel_item()
            .map(|i| i.inst.cfg.version.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    fn host(&self) -> String {
        self.settings.host_port().0
    }

    /// 启动时把"端口已在听"的实例标成运行中。
    fn attach_existing(&mut self, host: &str) -> usize {
        let mut n = 0;
        for item in self.insts.iter_mut() {
            if webui::listening(&host, item.inst.cfg.port) {
                item.inst.phase = instances::Phase::Running;
                item.inst.running_since = Some(std::time::Instant::now());
                item.inst.token = dsh::latest_token_at(&item.inst.log_path(), 0);
                n += 1;
            }
        }
        n
    }

    /// 把实例配置的改动写回设置并落盘。
    fn persist_instances(&mut self) {
        self.settings.instances =
            self.insts.iter().map(|i| i.inst.cfg.clone()).collect();
        // 第一个实例同时回写到老的 url / profile 字段：0.2.5 及以前的代码路径
        // （自检、托盘菜单、按 url 的旧逻辑）还读它们，保持它们与实例列表一致
        // 才不会出现"界面显示 A、旧逻辑按 B 干活"。
        if let Some(first) = self.settings.instances.first() {
            let host = self.host();
            self.settings.url = first.url(&host);
            self.settings.profile = first.profile.clone();
        }
        self.settings.save();
    }

    /// 新建一个实例（不动任何正在运行的进程）。
    fn add_instance(&mut self) {
        let name = trf!("实例 {}", self.insts.len() + 1);
        let settings = self.settings.clone();
        // id 要同时避开：① 当前实例列表 ② 磁盘上遗留的 homes/<id>（删实例不删数据）
        let taken = config::existing_instance_ids();
        let used_ids: Vec<String> =
            self.insts.iter().map(|i| i.inst.cfg.id.clone()).collect();
        // 自建实例只用自管版本，所以新建就带一个版本号（已装的最新那个，否则 npm 上的 latest）
        let version = settings.recommended_version();
        let idx = {
            // 先算出配置再推入：new_instance 需要看当前列表来挑端口
            let cfg = settings.new_instance(name, version, |id| {
                !used_ids.iter().any(|u| u == id) && !taken.contains(&config::safe_slug(id))
            });
            let port = cfg.port;
            config::log(&format!(
                "new instance {} on port {} version={}",
                cfg.id,
                port,
                if cfg.version.is_empty() { "(未定)" } else { &cfg.version }
            ));
            self.insts.push(InstItem::new(cfg));
            self.insts.len() - 1
        };
        self.selected = idx;
        // 新建之后停在「概览」：配置就在那一页，用户接着改端口/版本最顺手
        self.section = Section::Overview;
        self.persist_instances();
        self.rescan_plugins();
        let port = self.insts[idx].inst.cfg.port;
        self.toast = Some((trf!("已新建实例（端口 {}）", port), false));
    }

    /// 执行一次卸载：`None` = 全局安装，`Some((实例, 版本))` = 那个实例的那个版本。
    ///
    /// 全局安装的卸载要跑 npm（慢，放后台线程）；实例版本的卸载就是删目录（快，就地做）。
    fn run_uninstall(&mut self, target: Option<(String, String)>, ctx: &egui::Context) {
        match target {
            None => {
                if self.uninstalling_global {
                    return;
                }
                self.uninstalling_global = true;
                let ctx = ctx.clone();
                self.tasks.spawn(&ctx, || {
                    let mut lines: Vec<String> = Vec::new();
                    let mut sink = |s: String| lines.push(s);
                    let r = versions::uninstall_global(&mut sink);
                    let text = lines.join("\n");
                    match r {
                        Ok(msg) => TaskResult::Op(msg, Ok(text)),
                        Err(e) => TaskResult::Op(
                            tr!("卸载全局安装失败").to_string(),
                            Err(format!("{}\n{}", text, e)),
                        ),
                    }
                });
            }
            Some((id, version)) => {
                let name = self
                    .insts
                    .iter()
                    .find(|i| i.inst.cfg.id == id)
                    .map(|i| i.inst.cfg.name.clone())
                    .unwrap_or_else(|| id.clone());
                match versions::uninstall_managed(&id, &version) {
                    Ok(()) => {
                        // 如果这个实例正用着它，清掉版本号，让它回到"没版本"状态
                        // （界面会引导去装一个，而不是留着一个指向空目录的版本号）
                        if let Some(i) = self.insts.iter_mut().find(|i| i.inst.cfg.id == id) {
                            if i.inst.cfg.version.trim() == version {
                                i.inst.cfg.version.clear();
                            }
                        }
                        self.persist_instances();
                        self.managed_versions = dsh::managed_versions(&id);
                        self.toast =
                            Some((trf!("已把 {} 从「{}」卸载", version, name), false));
                    }
                    Err(e) => {
                        self.toast = Some((trf!("卸载 {} 失败：{}", version, e), true));
                    }
                }
            }
        }
    }

    /// 删除一个实例：先停掉它，再把它的整个目录（DSH_HOME + 它自己装的 DSH 版本）删掉。
    ///
    /// **按 id 删，不按下标**：确认框弹出后列表可能已经变了（用户又点了别处），
    /// 用下标会删错人。
    ///
    /// **全局实例不可删除**：它是"用全局安装那份 DSH"的唯一代表，
    /// 删了就没有对照物了。所以这里直接拒绝（UI 上也不会给删除按钮）。
    ///
    /// 删磁盘数据是不可逆的，所以界面上点「删除」时先走一次确认
    /// （见 `InstanceAct::Remove` 的处理），这里只负责真正执行。
    fn remove_instance_by_id(&mut self, id: &str) {
        let Some(idx) = self.insts.iter().position(|i| i.inst.cfg.id == id) else {
            return; // 已经不在了（重复确认 / 列表变了），当成已完成
        };
        if self.insts[idx].inst.cfg.is_global() {
            self.toast = Some((tr!("全局实例不能删除").to_string(), true));
            return;
        }
        let name = self.insts[idx].inst.cfg.name.clone();
        // 先停再删：Windows 上正跑着的进程会占住它自己的 DSH_HOME
        self.stop_instance(idx);
        // 删它自己的那一整个目录（home + 自装的版本）。失败就**不删配置**——
        // 否则界面上"已删除"，磁盘上却还留着几百 MB，用户再也找不到是谁占的。
        if let Err(e) = config::remove_instance_dir(id) {
            config::log(&format!("remove instance {} dir failed: {}", id, e));
            self.toast = Some((trf!("没能删掉 {} 的目录，实例保留：{}", name, e), true));
            return;
        }
        self.insts.remove(idx);
        // 其余实例可以删到一条不剩——全局实例还在，界面永远有得可选
        self.selected = self.sel();
        self.persist_instances();
        self.rescan_plugins();
        self.toast = Some((trf!("已删除实例 {}（含它的 DSH 主目录与已装版本）", name), false));
    }

    /// 启动一个实例。
    ///
    /// 已经在监听 → 只新开一个浏览器标签（不重复起服务）；这也是外部启动的实例
    /// 唯一能被"启动"按钮做的事。
    fn start_instance(&mut self, idx: usize, ctx: &egui::Context) {
        let host = self.host();
        let Some(item) = self.insts.get(idx) else { return };
        let name = item.inst.cfg.name.clone();
        let want_version = item.inst.cfg.version.trim().to_string();
        let is_global = item.inst.cfg.is_global();

        // 自建实例**只用自管版本**：没选版本就别启动。
        // 直接回落到全局安装会让人以为"这个实例和全局实例是一回事"，
        // 而它真正的差别（独立 DSH_HOME）又看不出来。引导去版本页选一个更诚实。
        if !is_global && want_version.is_empty() {
            self.section = Section::Versions;
            self.toast = Some((
                trf!("「{}」还没选版本：请在「版本」里装一个给它用", name),
                true,
            ));
            return;
        }

        // 指定了版本但自管目录里没装 → 先在后台装，装完自动接着启动。
        // 这个版本本来就是实例配置里选好的，所以不需要再"选用"（then_use = false）。
        if !want_version.is_empty()
            && dsh::installed_managed_version(&self.insts[idx].inst.cfg.id, &want_version).is_none()
        {
            self.install_version_for(idx, ctx, want_version, false);
            return;
        }

        let item = &mut self.insts[idx];
        let port = item.inst.cfg.port;
        let profile = item.inst.cfg.profile_or_default();
        let home = item.inst.home();
        let log = item.inst.log_path();

        // 已经在监听 → 只新开一个浏览器标签（这是「再点一次启动」的语义）
        if item.inst.phase != instances::Phase::Starting
            && webui::listening(&host, port)
        {
            if item.inst.phase != instances::Phase::Running {
                item.inst.phase = instances::Phase::Running;
                item.inst.running_since = Some(std::time::Instant::now());
            }
            if item.inst.token.is_none() {
                let off = item.inst.proc.as_ref().map(|p| p.log_offset).unwrap_or(0);
                item.inst.token = dsh::latest_token_at(&log, off);
            }
            let token = item.inst.token.clone();
            let url = item.inst.url(&host);
            match webui::open_web_ui(&url, token.as_deref()) {
                Ok(()) => {
                    self.toast =
                        Some((trf!("{} 已在运行，已在浏览器新开一个 Web UI", name), false))
                }
                Err(e) => self.toast = Some((e, true)),
            }
            return;
        }
        if item.inst.phase == instances::Phase::Starting {
            return; // 启动中，忽略重复点击
        }

        let version = want_version.clone();
        let version = if version.is_empty() { None } else { Some(version) };
        let owner = self.insts[idx].inst.cfg.id.clone();
        let entry = match dsh::resolve_entry_for(&owner, version.as_deref()) {
            Ok(e) => e,
            Err(msg) => {
                self.insts[idx].inst.phase = instances::Phase::Failed(msg.clone());
                self.toast = Some((msg, true));
                return;
            }
        };
        config::log(&format!(
            "starting instance {} on port {} via {} [{}]",
            name, port, entry.entry.display(), entry.source
        ));
        match dsh::spawn_in(&entry, &host, port, &profile, &home, &log) {
            Ok(spawned) => {
                let now = std::time::Instant::now();
                let item = &mut self.insts[idx];
                item.inst.token = None;
                item.inst.proc = Some(spawned);
                item.inst.phase = instances::Phase::Starting;
                // 与 instances 模块里的超时保持一致：DSH 要挂载整棵插件树，机器忙时明显变慢
                item.inst.boot_deadline =
                    Some(now + std::time::Duration::from_secs(INSTANCE_START_TIMEOUT_SECS));
                self.toast = Some((trf!("{} 正在启动…", name), false));
            }
            Err(msg) => {
                self.insts[idx].inst.phase = instances::Phase::Failed(msg.clone());
                self.toast = Some((msg, true));
            }
        }
    }

    /// 关闭一个实例。
    fn stop_instance(&mut self, idx: usize) {
        let host = self.host();
        let Some(item) = self.insts.get_mut(idx) else { return };
        let name = item.inst.cfg.name.clone();
        let _ = host;
        if let Some(mut p) = item.inst.proc.take() {
            config::log(&format!("stopping DSH tree pid={}", p.pid));
            p.shutdown();
        }
        item.inst.phase = instances::Phase::Stopped;
        item.inst.token = None;
        item.inst.running_since = None;
        item.inst.boot_deadline = None;
        self.toast = Some((trf!("已关闭 {}", name), false));
    }

    /// 打开某个实例的 Web UI。
    fn open_instance_ui(&mut self, idx: usize) {
        let host = self.host();
        let (url, token) = {
            let i = &self.insts[idx].inst;
            (i.url(&host), i.token.clone())
        };
        if !webui::listening(&host, self.insts[idx].inst.cfg.port) {
            self.toast = Some((
                trf!(
                    "{} 还没运行（{} 无法连接）。请先点「启动」。",
                    self.insts[idx].inst.cfg.name,
                    self.insts[idx].inst.cfg.port
                ),
                true,
            ));
            return;
        }
        match webui::open_web_ui(&url, token.as_deref()) {
            Ok(()) => {
                self.toast = Some((tr!("已在系统浏览器中打开 Web UI").into(), false))
            }
            Err(e) => self.toast = Some((e, true)),
        }
    }

    /// 把某个版本装到自管目录（后台跑 npm）。
    ///
    /// 从概览和版本页都可能触发，所以两处"正在装"的标记都要立起来——
    /// 否则另一处会显示成可再点一次（而 npm 正在写同一个目录）。
    ///
    /// `then_use`：装完之后**顺便让这个实例就用它**。
    /// 在某个实例的控制台里点「装到自管目录」，意思就是"我要这个实例跑这个版本"——
    /// 装完还得再点一次「用这个」的话，用户会以为没用（实际是没选）。
    fn install_version_for(
        &mut self,
        idx: usize,
        ctx: &egui::Context,
        version: String,
        then_use: bool,
    ) {
        if self.insts[idx].inst.installing.is_some() || self.installing_managed.is_some() {
            return;
        }
        self.insts[idx].inst.installing = Some(version.clone());
        self.insts[idx].inst.use_after_install = then_use;
        self.installing_managed = Some(version.clone());
        let name = self.insts[idx].inst.cfg.name.clone();
        // 装到**这个实例自己**的版本目录里（`instances/<id>/versions/<版本>`）：
        // 自建实例之间不共用版本文件，删实例时才能把它一起干净地删掉
        let owner = self.insts[idx].inst.cfg.id.clone();
        self.toast = Some((trf!("正在为 {} 安装 DSH {}…", name, version), false));
        let v = version.clone();
        self.tasks.spawn(ctx, move || {
            // 收集 npm 输出。注意 lines 会被闭包借用，所以这里用一个可变的 Vec
            // 贯穿整个流程，最后统一 join——不要在中途把 lines 移走。
            let mut lines: Vec<String> = Vec::new();
            let result = match dsh::resolve_node() {
                None => Err(tr!(
                    "未找到 Node.js。装指定版本的 DSH 需要 Node.js（自带 npm）。"
                )
                .to_string()),
                Some(node) => {
                    let mut sink = |s: String| lines.push(s);
                    dsh::install_managed_version(&owner, &node, &v, &mut sink)
                        .map(|e| e.display().to_string())
                }
            };
            let text = lines.join("\n");
            match result {
                Ok(entry) => TaskResult::VersionReady(
                    v,
                    Ok(trf!("{}\n\n入口：{}", text, entry)),
                ),
                Err(e) => TaskResult::VersionReady(v, Err(format!("{}\n{}", text, e))),
            }
        });
    }

    /// 实际生效的 profile：设置里为空时回落到默认 web
    /// （自定义模式允许输入框暂时为空，不能把空串传给 dsh）。
    fn profile(&self) -> String {
        if let Some(i) = self.sel_item() {
            return i.inst.cfg.profile_or_default();
        }
        let p = self.settings.profile.trim();
        if p.is_empty() {
            config::DEFAULT_PROFILE.to_string()
        } else {
            p.to_string()
        }
    }

    /// 选中实例的插件操作上下文（插件装在哪、用哪一份 dsh CLI、版本在谁的目录里）。
    fn ops(&self) -> plugins::Ops {
        let instance = self
            .sel_item()
            .map(|i| i.inst.cfg.id.clone())
            .unwrap_or_else(|| settings::GLOBAL_ID.to_string());
        plugins::Ops::new(
            &instance,
            &self.profile(),
            &self.selected_home(),
            self.selected_version().as_deref(),
        )
    }

    fn refresh_versions(&mut self, ctx: &egui::Context) {
        self.installed = versions::installed();
        self.tasks.spawn(ctx, || TaskResult::Versions(versions::fetch_remote()));
    }

    fn refresh_plugins(&mut self, ctx: &egui::Context) {
        self.rescan_plugins();
        self.reload_market(ctx);
        self.check_plugin_updates(ctx);
    }

    /// 重新扫描已装插件。
    ///
    /// 扫描只读本地文件（profile 的 package.json / node_modules / cordis.patch.yml
    /// 与全局 dsh 安装），毫秒级，所以直接在 UI 线程做——点了刷新马上就能看到结果。
    fn rescan_plugins(&mut self) {
        // 按**选中实例**的 DSH_HOME 扫：多实例各有一份 profile 与插件目录，
        // 用全局那套扫出来的会是别的实例的插件列表。
        let rep = self.ops().scan();
        for w in &rep.warnings {
            config::log(&format!("plugin scan: {}", w));
        }
        self.plugin_scan = rep;
    }

    /// 后台检查已装插件在 npm 上的最新版本。
    ///
    /// 只查"用户装的、而且能定位到包目录"的那些（GitHub / 本地路径装的自然查不到，
    /// 界面据此把「更新」置灰）。查完如果开着"自动更新"，会接着自动装一轮。
    fn check_plugin_updates(&mut self, ctx: &egui::Context) {
        if self.update_checking {
            return;
        }
        let names: Vec<String> = self
            .plugin_scan
            .plugins
            .iter()
            .filter(|p| p.dir.is_some())
            .map(|p| p.package.clone())
            .collect();
        if names.is_empty() {
            self.plugin_updates.clear();
            return;
        }
        self.update_checking = true;
        self.tasks.spawn(ctx, move || {
            let (found, notes) = plugins::fetch_latest(&names);
            TaskResult::PluginLatest(found, notes)
        });
    }

    /// 有新版可更新的插件：(包名, 最新版本)。
    fn outdated(&self) -> Vec<(String, String)> {
        self.plugin_scan
            .plugins
            .iter()
            .filter_map(|p| {
                let latest = self.plugin_updates.get(&p.package)?;
                plugins::is_newer(latest, &p.version).then(|| (p.package.clone(), latest.clone()))
            })
            .collect()
    }

    /// 开了"自动更新"就把能更新的都装到最新版（每次运行最多自动跑一轮）。
    fn maybe_auto_update(&mut self, ctx: &egui::Context) {
        if !self.settings.auto_update_plugins || self.auto_update_ran || self.plugin_busy.is_some() {
            return;
        }
        let targets = self.outdated();
        if targets.is_empty() {
            return;
        }
        self.auto_update_ran = true;
        let ops = self.ops();
        self.plugin_busy = Some(trf!("正在自动更新 {} 个插件…", targets.len()));
        config::log(&format!(
            "auto-updating {} plugin(s): {}",
            targets.len(),
            targets.iter().map(|(n, v)| format!("{}@{}", n, v)).collect::<Vec<_>>().join(", ")
        ));
        self.tasks.spawn(ctx, move || {
            let mut ok: Vec<String> = Vec::new();
            let mut bad: Vec<String> = Vec::new();
            for (name, latest) in &targets {
                match ops.update_to_latest(name) {
                    Ok(_) => {
                        let actual = ops
                            .installed_version(name)
                            .unwrap_or_else(|| tr!("未知").to_string());
                        ok.push(format!("{} {} → {}", name, latest, actual));
                    }
                    Err(e) => bad.push(format!("{}：{}", name, e)),
                }
            }
            let mut text = String::new();
            if !ok.is_empty() {
                text.push_str(&trf!("已更新：\n  {}\n", ok.join("\n  ")));
            }
            if !bad.is_empty() {
                text.push_str(&trf!("失败：\n  {}\n", bad.join("\n  ")));
            }
            if bad.is_empty() {
                TaskResult::Op(trf!("已自动更新 {} 个插件", ok.len()), Ok(text))
            } else {
                TaskResult::Op(
                    trf!("自动更新：{} 个成功、{} 个失败", ok.len(), bad.len()),
                    Err(text),
                )
            }
        });
    }

    /// 手动更新一个插件到最新版（后台跑 pnpm）。
    fn update_plugin(&mut self, ctx: &egui::Context, package: String, latest: Option<String>) {
        if self.plugin_busy.is_some() {
            return;
        }
        self.plugin_busy = Some(trf!("正在把 {} 更新到最新版…", package));
        let ops = self.ops();
        let label = package.clone();
        self.tasks.spawn(ctx, move || match ops.update_to_latest(&package) {
            Ok(text) => {
                // 报告**磁盘上实际装到的版本**：npm 的 latest 与 pnpm 落盘的版本
                // 可能差一档（pnpm 有最小发布年龄策略）
                let actual = ops.installed_version(&package);
                let tail = match (&actual, &latest) {
                    (Some(got), Some(want)) if got != want => {
                        trf!("（实际装到 {}，npm 的 latest 是 {}）", got, want)
                    }
                    (Some(got), _) => format!("（{}）", got),
                    (None, Some(want)) => trf!("（目标 {}）", want),
                    (None, None) => String::new(),
                };
                TaskResult::Op(trf!("{} 已更新到最新版{}", label, tail), Ok(text))
            }
            Err(e) => TaskResult::Op(trf!("更新 {} 失败", label), Err(e)),
        });
    }

    /// 插件行动作的分发。
    ///
    /// 「更新」在**自动更新关着**的时候要先确认（设置里那句"需要用户确认后才能更新"）；
    /// 开着就直接装——那是用户自己选的"自动"。
    fn run_installed_action(&mut self, ctx: &egui::Context, a: InstalledAction) {
        match a {
            InstalledAction::None => {}
            InstalledAction::Uninstall(pkg) => self.uninstall_plugin(ctx, pkg),
            InstalledAction::Toggle { label, ids, enabled } => {
                self.toggle_plugin(ctx, label, ids, enabled)
            }
            InstalledAction::Update { package, latest } => {
                if self.settings.auto_update_plugins {
                    self.update_plugin(ctx, package, latest);
                } else {
                    self.pending_confirm = Some(ConfirmAction::Update { package, latest });
                }
            }
            InstalledAction::OpenRepo(url) => {
                if let Err(e) = webui::open_url(&url) {
                    self.toast = Some((e, true));
                }
            }
        }
    }

    /// 待确认条：写在配置/装包之前问一句（确认 / 取消）。
    fn confirm_bar(&mut self, ui: &mut egui::Ui) {
        let Some(c) = self.pending_confirm.clone() else {
            return;
        };
        let ctx = ui.ctx().clone();
        egui::Frame::NONE
            .fill(theme::warn_soft())
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .stroke(egui::Stroke::new(1.0, theme::warn().gamma_multiply(0.4)))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_max_height(0.0); // 高度按内容走，别把整块地方都占了
                // 竖排：问题一段、按钮一段。横排时问题一长（比如删实例要把路径写清楚）
                // 就会把右侧按钮顶出窗口，用户根本点不到"确认"。
                ui.vertical(|ui| {
                    ui.set_max_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(c.question())
                            .size(13.0)
                            .color(theme::text()),
                    );
                    ui.add_space(6.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                        // 危险动作（删数据）用警示色，普通确认用主色
                        let (fill, text_col) = if c.is_destructive() {
                            (theme::err_solid(), egui::Color32::WHITE)
                        } else {
                            (theme::accent_solid(), egui::Color32::WHITE)
                        };
                        let ok = egui::Button::new(
                            egui::RichText::new(c.ok_label()).size(13.5).color(text_col),
                        )
                        .fill(fill)
                        .stroke(egui::Stroke::NONE);
                        if ui.add(ok).clicked() {
                            self.pending_confirm = None;
                            match c.clone() {
                                ConfirmAction::Update { package, latest } => {
                                    self.update_plugin(&ctx, package, latest)
                                }
                                // **不能在这里直接删**：确认条是在 console_overview 里画的，
                                // 此刻 self 还被那一帧借用着，而列表一变短，同帧后面记下的
                                // 下标就悬空了（曾经直接 panic：「len is 1 but the index is 1」）。
                                // 记成待办动作，等本帧绘制结束、在 ui() 里统一执行。
                                ConfirmAction::RemoveInstance { id, .. } => {
                                    self.pending_remove = Some(id)
                                }
                                // 卸载也要等绘制结束再做：删版本会改列表长度，
                                // 删全局安装会改 self.installed，都是同帧后面还要读的东西
                                ConfirmAction::UninstallGlobal => self.pending_uninstall = Some(None),
                                ConfirmAction::UninstallInstanceVersion { id, version, .. } => {
                                    self.pending_uninstall = Some(Some((id, version)))
                                }
                            }
                        }
                        if ui.button(tr!("取消")).clicked() {
                            self.pending_confirm = None;
                        }
                    });
                });
            });
        ui.add_space(8.0);
    }

    /// 重新抓插件市场（后台线程，旧列表先留在界面上，抓回来再替换）。
    fn reload_market(&mut self, ctx: &egui::Context) {
        if self.market_loading {
            return;
        }
        self.market_loading = true;
        self.tasks.spawn(ctx, || TaskResult::Market(plugins::fetch_market()));
    }

    /// 起停入口：作用于**当前选中的**实例。
    fn start_dsh(&mut self, ctx: &egui::Context) {
        let idx = self.sel();
        self.start_instance(idx, ctx);
    }

    /// 关闭当前选中的实例（整树）。
    fn stop_dsh(&mut self) {
        let idx = self.sel();
        self.stop_instance(idx);
    }

    /// 打开当前选中实例的 Web UI。
    fn open_web_ui(&mut self) {
        let idx = self.sel();
        self.open_instance_ui(idx);
    }

    /// 把所有自己拉起的实例都关掉（退出启动器时用）。
    fn stop_all_instances(&mut self) {
        for i in 0..self.insts.len() {
            if let Some(mut p) = self.insts[i].inst.proc.take() {
                config::log(&format!("stopping DSH tree pid={} on exit", p.pid));
                p.shutdown();
            }
            self.insts[i].inst.phase = instances::Phase::Stopped;
            self.insts[i].inst.token = None;
            self.insts[i].inst.running_since = None;
        }
    }

    /// 把**全局安装**切换/安装到某个版本。
    fn install_version(&mut self, ctx: &egui::Context, version: String) {
        if self.installing.is_some() {
            return;
        }
        self.installing = Some(version.clone());
        let v = version.clone();
        self.tasks.spawn(ctx, move || {
            let mut lines: Vec<String> = Vec::new();
            let mut sink = |s: String| lines.push(s);
            let r = versions::install_global(&v, &mut sink);
            let text = lines.join("\n");
            match r {
                Ok(spec) => TaskResult::Op(
                    trf!("全局安装已是 {}", v),
                    Ok(trf!("{}\n\n已执行: npm i -g {}", text, spec)),
                ),
                Err(e) => TaskResult::Op(trf!("切换 {} 失败", v), Err(format!("{}\n{}", text, e))),
            }
        });
    }

    /// 插件：安装（**装到选中实例**的 profile 里）。
    fn install_plugin(&mut self, ctx: &egui::Context, p: PluginInfo) {
        let spec = if !p.npm.is_empty() { p.npm.clone() } else { p.install.clone() };
        if spec.is_empty() {
            self.toast = Some((tr!("该插件没有可用的安装标识").into(), true));
            return;
        }
        self.plugin_busy = Some(trf!("正在安装 {} …", p.name));
        let ops = self.ops();
        let label = p.name.clone();
        self.tasks.spawn(ctx, move || match ops.install(&spec) {
            Ok(text) => TaskResult::Op(trf!("已安装 {}", label), Ok(text)),
            Err(e) => TaskResult::Op(trf!("安装 {} 失败", label), Err(e)),
        });
    }

    /// 插件：卸载（作用于选中实例）。
    fn uninstall_plugin(&mut self, ctx: &egui::Context, package: String) {
        self.plugin_busy = Some(trf!("正在卸载 {} …", package));
        let ops = self.ops();
        let label = package.clone();
        self.tasks.spawn(ctx, move || match ops.uninstall(&package) {
            Ok(text) => TaskResult::Op(trf!("已卸载 {}", label), Ok(text)),
            Err(e) => TaskResult::Op(trf!("卸载 {} 失败", label), Err(e)),
        });
    }

    /// 插件：启用/禁用（一个包可能对应多个 cordis 条目 id，一起改）。
    ///
    /// 改的是**选中实例**的 `cordis.patch.yml`——多实例各有自己的 profile 目录。
    fn toggle_plugin(&mut self, ctx: &egui::Context, label: String, ids: Vec<String>, enabled: bool) {
        match self.ops().toggle_ids(&ids, enabled) {
            Ok(()) => {
                self.rescan_plugins();
                self.toast = Some((
                    trf!("{} 已{}", label, if enabled { "启用" } else { "禁用" }),
                    false,
                ));
                let _ = ctx;
            }
            Err(e) => self.toast = Some((e, true)),
        }
    }

    /// 处理托盘动作。
    fn handle_tray(&mut self, ctx: &egui::Context) {
        while let Some(a) = tray::take_action() {
            match a {
                TrayAction::ToggleWindow => self.toggle_window(),
                // 托盘菜单作用于**当前选中的实例**：多实例下"启动/关闭"必须有个明确目标
                TrayAction::StartDsh => self.start_dsh(ctx),
                TrayAction::StopDsh => self.stop_dsh(),
                TrayAction::OpenWebUi => self.open_web_ui(),
                TrayAction::SetCloseToTray(to_tray) => {
                    self.settings.close_action =
                        if to_tray { CloseAction::Tray } else { CloseAction::Exit };
                    tray::set_close_to_tray(to_tray);
                    self.settings.save();
                }
                TrayAction::Quit => self.quit = true,
            }
            let _ = ctx;
        }
    }

    /// 显示/隐藏主窗口（在托盘里切换）。
    fn toggle_window(&mut self) {
        if !self.tray_ok {
            return;
        }
        self.hidden_to_tray = !self.hidden_to_tray;
        tray::set_window_visible(!self.hidden_to_tray);
    }

    /// 消费后台任务结果。
    fn poll_tasks(&mut self, ctx: &egui::Context) {
        while let Ok(r) = self.tasks.rx.try_recv() {
            self.tasks.inflight = self.tasks.inflight.saturating_sub(1);
            match r {
                TaskResult::Versions(Ok(list)) => {
                    self.versions = list;
                    self.versions_fetched = true;
                }
                TaskResult::Versions(Err(e)) => {
                    if !self.versions_fetched {
                        self.toast = Some((trf!("版本列表获取失败：{}", e), true));
                    }
                }
                TaskResult::Market(Ok(list)) => {
                    self.market = list;
                    self.market_loaded = true;
                    self.market_loading = false;
                }
                TaskResult::Market(Err(e)) => {
                    self.market_loading = false;
                    self.toast = Some((trf!("插件市场加载失败：{}", e), true));
                }
                TaskResult::Github(Ok(list)) => {
                    self.github_results = list;
                    self.github_loading = false;
                }
                TaskResult::Github(Err(e)) => {
                    self.github_loading = false;
                    self.toast = Some((trf!("GitHub 搜索失败：{}", e), true));
                }
                TaskResult::PluginLatest(found, notes) => {
                    self.plugin_updates = found;
                    self.update_checking = false;
                    for n in &notes {
                        config::log(&format!("plugin update check: {}", n));
                    }
                    self.maybe_auto_update(ctx);
                }
                TaskResult::VersionReady(ver, res) => {
                    // 哪个实例在等这个版本
                    let waiting: Vec<usize> = self
                        .insts
                        .iter()
                        .enumerate()
                        .filter(|(_, i)| i.inst.installing.as_deref() == Some(ver.as_str()))
                        .map(|(idx, _)| idx)
                        .collect();
                    let use_it: Vec<usize> = waiting
                        .iter()
                        .copied()
                        .filter(|idx| self.insts[*idx].inst.use_after_install)
                        .collect();
                    for idx in &waiting {
                        self.insts[*idx].inst.installing = None;
                        self.insts[*idx].inst.use_after_install = false;
                    }
                    self.installing_managed = None;
                    self.managed_versions =
                        dsh::managed_versions(&self.insts[self.sel()].inst.cfg.id);
                    match res {
                        Ok(text) => {
                            self.last_output = Some(text);
                            // 「装到自管目录」是"我要这个实例跑这个版本"：装完就切过去，
                            // 别让用户再点一次（否则界面还显示"全局安装"，像是没生效）。
                            for idx in &use_it {
                                self.insts[*idx].inst.cfg.version = ver.clone();
                            }
                            if !use_it.is_empty() {
                                self.persist_instances();
                            }
                            // 装完**接着启动**：用户点的是「启动」/「装到自管目录」，
                            // 不该让他再点一次。安装要几分钟（实测 0.1.7-rc.1 约 220 秒），
                            // 中途让「启动」可点会在 npm 还没写完时就把进程拉起来
                            // ——所以安装期间那个按钮是禁用的，这里补上后半程。
                            for idx in waiting {
                                self.toast = Some((
                                    trf!("DSH {} 已装好，正在启动…", ver),
                                    false,
                                ));
                                self.start_instance(idx, ctx);
                            }
                        }
                        Err(e) => {
                            self.toast = Some((trf!("安装 DSH {} 失败", ver), true));
                            self.last_output = Some(e);
                        }
                    }
                }
                TaskResult::Op(title, res) => {
                    self.installing = None;
                    self.uninstalling_global = false;
                    self.plugin_busy = None;
                    match res {
                        Ok(text) => {
                            self.toast = Some((title, false));
                            self.last_output = Some(text);
                        }
                        Err(e) => {
                            self.toast = Some((title, true));
                            self.last_output = Some(e);
                        }
                    }
                    self.installed = versions::installed();
                    self.rescan_plugins();
                    // 装/卸/更新之后重新看一眼最新版本（自动更新也在这里被触发）
                    self.check_plugin_updates(ctx);
                }
            }
        }
        if self.tasks.inflight > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// 有没有"起来了但 token 还没抓到"的实例（token 迟到时要持续重绘去补抓）。
    fn any_waiting_token(&self) -> bool {
        self.insts.iter().any(|i| {
            i.inst.phase == instances::Phase::Running
                && i.inst.token.is_none()
                && i.inst.proc.is_some()
        })
    }

    /// 轮询所有实例：就绪 / 超时 / 提前退出 / 被外部关掉。
    ///
    /// 逻辑本身在 [`instances::poll`]，这里只负责把结果反映到界面上
    /// （toast、失败详情、自动开浏览器）。
    fn poll_dsh(&mut self) {
        let host = self.host();
        let mut events = Vec::new();
        let mut starting = 0usize;
        let mut newly_up: Vec<usize> = Vec::new();

        for (idx, item) in self.insts.iter_mut().enumerate() {
            let before = item.inst.phase.clone();
            // 单实例轮询：把这一项单独跑一遍 poll，拿到的就是它的迁移
            let (ev, st) = instances::poll(std::slice::from_mut(&mut item.inst), &host);
            events.extend(ev);
            if st > 0 {
                starting += 1;
            }
            if before == instances::Phase::Starting
                && item.inst.phase == instances::Phase::Running
            {
                newly_up.push(idx);
            }
        }

        // 失败详情写进"最近操作输出"，便于对照
        for item in self.insts.iter() {
            if let instances::Phase::Failed(msg) = &item.inst.phase {
                self.last_output = Some(msg.clone());
            }
        }

        if let Some(first) = events.first() {
            self.toast = Some((events.join("\n"), false));
            let _ = first;
        }

        // 起来了就按设置自动开浏览器（只对刚就绪的那些）
        for idx in newly_up {
            let (name, url, token) = {
                let i = &self.insts[idx].inst;
                (i.cfg.name.clone(), i.url(&host), i.token.clone())
            };
            config::log(&format!("instance {} web UI is up", name));
            self.toast = Some((trf!("{} 已启动", name), false));
            if self.settings.auto_open_browser {
                let _ = webui::open_web_ui(&url, token.as_deref());
            }
        }

        self.starting_count = starting;
    }
}

// ------------------------------------------------------------------ eframe::App

impl eframe::App for App {
    /// 每帧 UI 之前：处理后台结果、DSH 轮询、托盘动作。
    /// （egui 0.36 的 eframe 把「非绘制逻辑」与「绘制」拆成 logic / ui 两步。）
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_tasks(ctx);
        self.poll_dsh();
        self.handle_tray(ctx);

        if self.quit {
            self.shutdown();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.starting_count > 0 || self.any_waiting_token() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // 关窗兜底：✕ 的处理见 close_requested()
        if ctx.input(|i| i.viewport().close_requested()) && !self.hidden_to_tray {
            if self.tray_ok && self.settings.close_action == CloseAction::Tray {
                self.hidden_to_tray = true;
                tray::set_window_visible(false);
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                config::log("window hidden to tray");
            } else {
                self.shutdown();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.quit {
            return;
        }
        let ctx = ui.ctx().clone();

        self.top_bar(ui);

        let nav = egui::Panel::left("nav")
            .exact_size(190.0)
            .resizable(false)
            .frame(
                egui::Frame::NONE
                    .fill(theme::sidebar())
                    .inner_margin(egui::Margin::symmetric(12, 14)),
            )
            .show(ui, |ui| self.sidebar(ui));
        // 侧栏右侧分隔线
        let nr = nav.response.rect;
        ui.painter().vline(
            nr.right() - 0.5,
            nr.y_range(),
            egui::Stroke::new(1.0, theme::border()),
        );

        // 提示条配色：tuple 的第二项为 true 表示「这是错误」
        if let Some((_, is_err)) = self.toast.as_ref() {
            let bg = if *is_err { theme::err_soft() } else { theme::ok_soft() };
            egui::Panel::bottom("toast")
                .frame(
                    egui::Frame::NONE
                        .fill(bg)
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show(ui, |ui| self.toast_bar(ui));
        }

        // 实例行上的动作要在绘制结束后再执行（绘制期间不能同时可变借用 self）
        let pending = self.pending_inst_act.take();
        let pending_rm = self.pending_remove.take();
        let pending_un = self.pending_uninstall.take();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::bg()).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| match self.page {
                Page::Launcher => self.tab_launcher_settings(ui),
                Page::Console => self.tab_console(ui),
            });

        // 删除实例：确认条只负责"记下来"，真正的删除在这里做——
        // 绘制已经结束，列表怎么变都不会再有悬空下标
        if let Some(id) = pending_rm {
            self.remove_instance_by_id(&id);
        }
        if let Some(target) = pending_un {
            self.run_uninstall(target, &ctx);
        }

        if let Some((idx, act)) = pending {
            self.run_instance_act(idx, act, &ctx);
        }

        let _ = ctx;
    }
}

// ------------------------------------------------------------------ 主题

mod theme {
    use egui::Color32;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// 当前风格：false = 浅色，true = 深色。
    /// 用原子量而不是把调色板传遍全场——配色是全局观感，本来就该是全局状态；
    /// 切换后下一帧重绘就会取到新值（切风格时会 request_repaint）。
    static DARK: AtomicBool = AtomicBool::new(false);

    pub fn is_dark() -> bool {
        DARK.load(Ordering::Relaxed)
    }

    pub fn set_dark(dark: bool) {
        DARK.store(dark, Ordering::Relaxed);
    }

    fn pick(light: u32, dark: u32) -> Color32 {
        let v = if is_dark() { dark } else { light };
        Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, (v & 0xff) as u8)
    }

    // —— 画布与表面 ——
    /// 窗口画布（最底层）
    pub fn bg() -> Color32 {
        pick(0xf3f5fa, 0x101319)
    }
    /// 顶栏 / 侧栏
    pub fn sidebar() -> Color32 {
        pick(0xffffff, 0x1a1f28)
    }
    /// 卡片表面
    pub fn card() -> Color32 {
        pick(0xffffff, 0x1a1f28)
    }
    /// 控件底（次要按钮 / 输入框 / 提示条）
    pub fn surface() -> Color32 {
        pick(0xf1f4f9, 0x232a35)
    }
    /// 控件悬停底
    pub fn surface_hover() -> Color32 {
        pick(0xe6ecf6, 0x2d3644)
    }

    // —— 主色与语义色 ——
    pub fn accent() -> Color32 {
        pick(0x2f6feb, 0x74a9ff)
    }
    pub fn accent_soft() -> Color32 {
        pick(0xe8f0ff, 0x1e2d49)
    }
    pub fn ok() -> Color32 {
        pick(0x148a4a, 0x5fd684)
    }
    pub fn ok_soft() -> Color32 {
        pick(0xe6f7ec, 0x14301f)
    }
    pub fn warn() -> Color32 {
        pick(0xb0690a, 0xf3c65c)
    }
    pub fn warn_soft() -> Color32 {
        pick(0xfdf3e2, 0x33280f)
    }
    pub fn err() -> Color32 {
        pick(0xd6332e, 0xff8279)
    }
    pub fn err_soft() -> Color32 {
        pick(0xfdeceb, 0x3a1c1b)
    }

    // —— 实色：**按钮填充**专用 ——
    // 上面那几个是"当前景用"的（深色模式下要够亮才看得清文字），
    // 但拿它们当按钮底再配白字就糊了（亮蓝上的白字对比度只有 ~2:1）。
    // 所以按钮底色单独给一组饱和度够、白字压得住的实色。
    pub fn accent_solid() -> Color32 {
        pick(0x2f6feb, 0x3574ee)
    }
    pub fn ok_solid() -> Color32 {
        pick(0x148a4a, 0x178a4a)
    }
    pub fn err_solid() -> Color32 {
        pick(0xd6332e, 0xdc3b36)
    }

    // —— 文字与描边 ——
    /// 主文字：浅色下近黑，深色下近白——**两边都不用灰字**
    pub fn text() -> Color32 {
        pick(0x182230, 0xf7f9fd)
    }
    /// 次要文字：浅色下中灰（白底上足够清），深色下用**很亮的浅色**，
    /// 不做"深灰压黑底"那种看不清的处理
    pub fn dim() -> Color32 {
        pick(0x5b6778, 0xd9e2f0)
    }
    /// 描边 / 分隔线
    pub fn border() -> Color32 {
        pick(0xdfe5ee, 0x333d4d)
    }
}

/// 卡片的投影：浅色下很轻（白卡浮起），深色下加重一点（黑底上靠它分层）。
fn card_shadow() -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 2],
        blur: if theme::is_dark() { 16 } else { 10 },
        spread: 0,
        color: egui::Color32::from_black_alpha(if theme::is_dark() { 90 } else { 18 }),
    }
}

/// 应用整套外观：当前风格（浅/深）、字号、控件配色。
/// 切换风格后调用一次即可，下一帧全部生效。
fn apply_theme(ctx: &egui::Context) {
    use egui::{CornerRadius, Stroke, Theme};

    // 风格由设置决定，不跟随系统：避免窗口标题栏与内容一亮一暗的割裂
    if theme::is_dark() {
        ctx.set_theme(egui::ThemePreference::Dark);
    } else {
        ctx.set_theme(egui::ThemePreference::Light);
    }

    let mut v = if theme::is_dark() { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = theme::bg();
    v.window_fill = theme::card();
    v.window_stroke = Stroke::new(1.0, theme::border());
    v.extreme_bg_color = theme::surface(); // 输入框底色
    v.faint_bg_color = theme::surface();
    v.hyperlink_color = theme::accent();
    v.window_corner_radius = CornerRadius::same(12);
    v.selection.bg_fill = theme::accent().gamma_multiply(0.28);
    v.selection.stroke = Stroke::new(1.0, theme::accent());

    let radius = CornerRadius::same(8);
    // 非交互（标签、卡片内的框）
    v.widgets.noninteractive.bg_fill = theme::card();
    v.widgets.noninteractive.weak_bg_fill = theme::card();
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, theme::border());
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, theme::text());
    v.widgets.noninteractive.corner_radius = radius;
    // 普通（次要按钮、勾选框）
    v.widgets.inactive.bg_fill = theme::surface();
    v.widgets.inactive.weak_bg_fill = theme::surface();
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, theme::border());
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, theme::text());
    v.widgets.inactive.corner_radius = radius;
    // 悬停
    v.widgets.hovered.bg_fill = theme::surface_hover();
    v.widgets.hovered.weak_bg_fill = theme::surface_hover();
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, theme::accent().gamma_multiply(0.5));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, theme::text());
    v.widgets.hovered.corner_radius = radius;
    // 按下 / 打开
    v.widgets.active.bg_fill = theme::accent_soft();
    v.widgets.active.weak_bg_fill = theme::accent_soft();
    v.widgets.active.bg_stroke = Stroke::new(1.0, theme::accent());
    v.widgets.active.fg_stroke = Stroke::new(1.0, theme::text());
    v.widgets.active.corner_radius = radius;
    v.widgets.open.bg_fill = theme::surface();
    v.widgets.open.weak_bg_fill = theme::surface();
    v.widgets.open.bg_stroke = Stroke::new(1.0, theme::border());
    v.widgets.open.fg_stroke = Stroke::new(1.0, theme::text());
    v.widgets.open.corner_radius = radius;

    // 明暗两套都设成同一份，万一系统主题在运行中切换也不会跳成别的样子
    ctx.set_visuals_of(Theme::Light, v.clone());
    ctx.set_visuals_of(Theme::Dark, v);

    // 字号整体调大一档，按钮/输入框这些标准控件才跟得上
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (egui::TextStyle::Heading, egui::FontId::proportional(21.0)),
            (egui::TextStyle::Body, egui::FontId::proportional(14.5)),
            (egui::TextStyle::Button, egui::FontId::proportional(14.5)),
            (egui::TextStyle::Small, egui::FontId::proportional(12.5)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
}

// ------------------------------------------------------------------ 绘制

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let panel = egui::Panel::top("top")
            .exact_size(62.0)
            .frame(
                egui::Frame::NONE
                    .fill(theme::sidebar())
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let tex = icon::logo_texture(ui.ctx(), theme::is_dark());
                    ui.add(egui::Image::new(&tex).fit_to_exact_size(egui::vec2(32.0, 32.0)));
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new("DSH Launch Console")
                                .size(17.0)
                                .strong()
                                .color(theme::text()),
                        );
                        ui.label(
                            egui::RichText::new(tr!("DeepSeek Harness 启动器"))
                                .size(12.5)
                                .color(theme::dim()),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 多实例下的总体状态：能跑几个、在跑几个
                        let running = self
                            .insts
                            .iter()
                            .filter(|i| i.inst.phase == instances::Phase::Running)
                            .count();
                        let starting = self
                            .insts
                            .iter()
                            .any(|i| i.inst.phase == instances::Phase::Starting);
                        let failed = self
                            .insts
                            .iter()
                            .any(|i| matches!(i.inst.phase, instances::Phase::Failed(_)));
                        // 实例多起来时把数量也说清楚，不然"运行中"看不出是几个
                        let show_count = self.insts.len() > 1;
                        let (fg, bg, label) = if starting {
                            (theme::warn(), theme::warn_soft(), tr!("启动中…").to_string())
                        } else if running > 0 {
                            (
                                theme::ok(),
                                theme::ok_soft(),
                                if show_count {
                                    trf!("运行中 {}/{}", running, self.insts.len())
                                } else {
                                    tr!("运行中").to_string()
                                },
                            )
                        } else if failed {
                            (theme::err(), theme::err_soft(), tr!("启动失败").to_string())
                        } else {
                            (theme::dim(), theme::surface(), tr!("未启动").to_string())
                        };
                        status_chip(ui, &label, fg, bg);
                    });
                });
            });

        // 顶栏底部分隔线（比整圈描边干净）
        let r = panel.response.rect;
        ui.painter()
            .hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, theme::border()));
    }

    /// 侧栏：**全局实例** + **新建实例** + **其余实例列表**，最下方是**设置**。
    ///
    /// 全局实例单独拎出来放在最上面：它不是"又一个实例"，而是全局那一份的代表，
    /// 永远存在、不可删。新建实例紧跟在它下面（"再加一个"的动作紧挨着"已有的基准"），
    /// 之后才是自己建的实例列表。设置放最底部——它偶尔才动一次，不该占着视线。
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);

        let global_ver = self.installed.global.clone();
        let mut picked: Option<usize> = None;

        // —— 全局实例（固定，永远排最上面）——
        if let Some(gidx) = self.insts.iter().position(|i| i.inst.cfg.is_global()) {
            let item = &self.insts[gidx];
            let selected = self.page == Page::Console && self.sel() == gidx;
            let dot = phase_dot(&item.inst.phase);
            let hint = trf!(
                "{}:{} · {} · {}",
                self.host(),
                item.inst.cfg.port,
                version_text(item.inst.configured_version(), global_ver.as_deref()),
                item.inst.status_text()
            );
            if sidebar_item(ui, selected, &item.inst.cfg.name, &hint, dot) {
                picked = Some(gidx);
            }
        }

        // —— 新建实例 ——
        // 不带圆点：那个圆点表示"这个实例跑没跑"，而这一项是个**动作**、不是实例，
        // 给它一个绿点会让人以为"已经有一个叫新建实例的在运行"。
        if sidebar_item(ui, false, tr!("＋  新建实例"), tr!("另起一个独立的 DSH"), None) {
            self.add_instance();
            self.page = Page::Console;
        }

        // —— 其余实例（自己建的；多了就滚动）——
        let others: Vec<usize> = (0..self.insts.len())
            .filter(|i| !self.insts[*i].inst.cfg.is_global())
            .collect();
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(trf!("其它实例（{}）", others.len()))
                .size(11.5)
                .color(theme::dim()),
        );
        ui.add_space(4.0);

        let list_h = (ui.available_height() - 60.0).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("sidebar_instances")
            .max_height(list_h)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for idx in others {
                    let item = &self.insts[idx];
                    let selected = self.page == Page::Console && self.sel() == idx;
                    let dot = phase_dot(&item.inst.phase);
                    let hint = trf!(
                        "{}:{} · {} · {}",
                        self.host(),
                        item.inst.cfg.port,
                        version_text(item.inst.configured_version(), global_ver.as_deref()),
                        item.inst.status_text()
                    );
                    if sidebar_item(ui, selected, &item.inst.cfg.name, &hint, dot) {
                        picked = Some(idx);
                    }
                    ui.add_space(4.0);
                }
            });
        if let Some(idx) = picked {
            if self.sel() != idx {
                self.selected = idx;
                self.rescan_plugins();
            }
            self.page = Page::Console;
        }

        // —— 最下方：设置（自下而上摆，保证贴着底边）——
        // 用的是"从底边往上排"的布局，所以**先摆的在最下面**：
        // 版本号在最底下，设置压在它上面。
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .size(11.0)
                    .color(theme::dim()),
            );
            ui.add_space(4.0);
            if sidebar_item(ui, self.page == Page::Launcher, tr!("设置"), "", None) {
                self.page = Page::Launcher;
            }
        });
    }

    // ------------------------------------------------------ 控制台

    // ------------------------------------------------------ 实例控制台

    /// 实例控制台 = **选中实例的设置页**。
    ///
    /// 一个实例的全部事情都在这一页：状态与启停、实例配置（名字/端口/profile/
    /// 版本/DSH 主目录）、版本管理、插件管理。用顶部分段切换，因为它们都是
    /// 同一个实例的属性——拆成四个侧栏页面会让人反复跳。
    fn tab_console(&mut self, ui: &mut egui::Ui) {
        // 确认条必须在**取 idx / 画任何东西之前**渲染：用户在这里点「删除」时，
        // 它会当场把实例从列表里移除，于是本帧后面记下的 idx 就成了越界下标
        // （曾经就是这样：删完继续 self.insts[1]，len 只剩 1 → 直接 panic 退出）。
        self.confirm_bar(ui);

        let idx = self.sel();
        let Some(item) = self.insts.get(idx) else {
            ui.label(tr!("还没有实例。用左侧栏的「＋ 新建实例」加一个。"));
            return;
        };
        let name = item.inst.cfg.name.clone();
        let status = item.inst.status_text();
        let dot = match &item.inst.phase {
            instances::Phase::Running => theme::ok(),
            instances::Phase::Starting => theme::warn(),
            instances::Phase::Failed(_) => theme::err(),
            instances::Phase::Stopped => theme::dim(),
        };

        // —— 顶部：这是哪个实例 ——
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 6.0, dot);
            ui.add_space(4.0);
            ui.heading(egui::RichText::new(tr!("控制台")).size(21.0).color(theme::text()));
            ui.label(
                egui::RichText::new(format!("· {}", name))
                    .size(15.0)
                    .color(theme::dim()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(status).size(12.5).color(dot));
            });
        });
        ui.add_space(8.0);

        // —— 分段切换：同一个实例的三块 ——
        ui.horizontal(|ui| {
            let tabs = [
                (Section::Overview, tr!("概览")),
                (Section::Versions, tr!("版本")),
                (Section::Plugins, tr!("插件")),
            ];
            for (sec, label) in tabs {
                let selected = self.section == sec;
                if ui.selectable_label(selected, egui::RichText::new(label).size(14.0)).clicked() {
                    self.section = sec;
                }
            }
        });
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .id_salt(("console_body", idx, self.section as u8))
            .auto_shrink([false, false])
            .show(ui, |ui| match self.section {
                Section::Overview => self.console_overview(ui, idx),
                Section::Versions => self.console_versions(ui),
                Section::Plugins => self.console_plugins(ui),
            });
    }

    /// 概览：该实例的状态、启停、以及它的全部配置。
    ///
    /// 实例行上的动作要在绘制结束后执行（绘制期间已经可变借用了 self），
    /// 所以这里把动作存进 `pending_inst_act`，由 `ui()` 在绘制后统一执行。
    fn console_overview(&mut self, ui: &mut egui::Ui, idx: usize) {
        // 兜底：idx 来自本帧早些时候算出的选中项，而这中间可能已经有人把实例删了
        // （确认条就是干这个的）。宁可这一帧不画，也不能越界 panic——
        // GUI 一 panic 就是整个窗口消失，用户只看到"闪退"。
        if idx >= self.insts.len() {
            return;
        }
        let host = self.host();
        // 全局安装那一份的版本号：没钉版本的实例显示的就是它，
        // 所以要在借用 self.insts 之前先取出来
        let global_ver = self.installed.global.clone();
        let item = &self.insts[idx];
        let inst = &item.inst;
        let running = matches!(inst.phase, instances::Phase::Running);
        let starting = matches!(inst.phase, instances::Phase::Starting);
        let live = running || starting;
        let installing = inst.installing.is_some();
        let stoppable = inst.stoppable();

        // —— 状态卡 ——
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(inst.status_text())
                        .size(15.5)
                        .strong()
                        .color(theme::text()),
                );
                if let Some(since) = inst.running_since {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(trf!("已运行 {}", fmt_dur(since.elapsed().as_secs())))
                                .size(13.0)
                                .color(theme::dim()),
                        );
                    });
                }
            });
            ui.add_space(6.0);
            // 版本只在这里**显示**，改它去「版本」分段——那才是管版本的地方。
            // 跳转按钮跟在信息行同一行，省得用户对着只读的一行找"在哪儿改"。
            // 全局实例不给这个按钮：它固定用全局安装那一份，跳过去也只有一句
            // "不能改版本"，不如不引导。
            let global_ver = self.installed.global.clone();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(trf!(
                        "{}   profile {}   版本 {}",
                        inst.url(&host),
                        inst.cfg.profile_or_default(),
                        version_text(inst.configured_version(), global_ver.as_deref())
                    ))
                    .size(12.5)
                    .color(theme::dim()),
                );
                if inst.cfg.is_global() {
                    ui.label(
                        egui::RichText::new(tr!("（固定）"))
                            .size(11.5)
                            .color(theme::dim()),
                    )
                    .on_hover_text(tr!(
                        "全局实例固定用全局安装的 DSH。要跑别的版本，新建一个实例。"
                    ));
                } else if ui
                    .small_button(tr!("改版本 →"))
                    .on_hover_text(tr!("到「版本」分段选择这个实例用哪个 DSH 版本"))
                    .clicked()
                {
                    self.section = Section::Versions;
                }
            });

            if let instances::Phase::Failed(msg) = &inst.phase {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(msg).size(12.5).color(theme::err()));
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let can_start = !starting && !installing;
                let start_label = if live {
                    tr!("▶  打开 Web UI")
                } else {
                    tr!("▶  启动")
                };
                let start_btn = egui::Button::new(
                    egui::RichText::new(start_label)
                        .size(15.0)
                        .strong()
                        .color(if can_start { egui::Color32::WHITE } else { theme::dim() }),
                )
                .fill(if !can_start {
                    theme::surface()
                } else if live {
                    theme::accent_solid()
                } else {
                    theme::ok_solid()
                })
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(180.0, 40.0))
                .corner_radius(10.0);
                if ui
                    .add_enabled(can_start, start_btn)
                    .on_hover_text(if installing {
                        tr!("正在安装这个版本，装好会自动启动")
                    } else if live {
                        tr!("已在运行：再点只会在浏览器新开一个 Web UI，不会重复起服务")
                    } else {
                        tr!("隐藏终端启动这个实例")
                    })
                    .clicked()
                {
                    self.pending_inst_act = Some((idx, InstanceAct::Start));
                }

                ui.add_space(8.0);
                let stop_btn = egui::Button::new(
                    egui::RichText::new(tr!("■  关闭"))
                        .size(15.0)
                        .strong()
                        .color(if stoppable { egui::Color32::WHITE } else { theme::dim() }),
                )
                .fill(if stoppable { theme::err_solid() } else { theme::surface() })
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(140.0, 40.0))
                .corner_radius(10.0);
                if ui
                    .add_enabled(stoppable, stop_btn)
                    .on_disabled_hover_text(tr!(
                        "这个 DSH 不是本启动器启动的，为避免误杀，启动器不会结束它"
                    ))
                    .clicked()
                {
                    self.pending_inst_act = Some((idx, InstanceAct::Stop));
                }

                ui.add_space(8.0);
                if ui
                    .add_enabled(
                        live,
                        egui::Button::new(egui::RichText::new(tr!("🌐  浏览器")).size(15.0))
                            .min_size(egui::vec2(120.0, 40.0))
                            .corner_radius(10.0),
                    )
                    .clicked()
                {
                    self.pending_inst_act = Some((idx, InstanceAct::OpenUi));
                }

                if installing {
                    ui.add_space(8.0);
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(trf!(
                            "正在安装 {}…",
                            inst.installing.clone().unwrap_or_default()
                        ))
                        .size(13.0)
                        .color(theme::warn()),
                    );
                }
            });
        });

        ui.add_space(12.0);

        // —— 实例配置 ——
        card(ui, |ui| {
            ui.label(
                egui::RichText::new(tr!("实例配置"))
                    .size(15.0)
                    .strong()
                    .color(theme::text()),
            );
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(tr!("改完即时保存；正在运行的实例要重启才生效"))
                    .size(12.0)
                    .color(theme::dim()),
            );
            ui.add_space(8.0);

            let mut name = inst.cfg.name.clone();
            let mut port = inst.cfg.port.to_string();
            // profile 不再直接改：交给 profile_combo 里的输入框，避免两处同时可变借用
            let profile = inst.cfg.profile.clone();
            // 版本在概览里只读（改它去「版本」分段），所以不需要可变副本
            let mut home = inst.cfg.home.clone();
            let mut own_home = inst.cfg.own_home;
            let mut changed: Option<(&'static str, String)> = None;
            // 全局实例：名字 / 版本 / 主目录都固定，不给改
            let is_global = inst.cfg.is_global();

            egui::Grid::new(("inst_cfg", inst.cfg.id.as_str()))
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(tr!("名字")).size(12.5));
                    if is_global {
                        // 全局实例的名字是固定的：它不是普通实例，
                        // 改成一个别的名字只会让"全局"这个说法失去指代
                        ui.label(
                            egui::RichText::new(inst.cfg.name.clone())
                                .size(13.0)
                                .strong()
                                .color(theme::text()),
                        );
                    } else if ui
                        .add(egui::TextEdit::singleline(&mut name).desired_width(260.0))
                        .changed()
                    {
                        changed = Some(("name", name.clone()));
                    }
                    ui.end_row();

                    ui.label(egui::RichText::new(tr!("端口")).size(12.5));
                    ui.horizontal(|ui| {
                        if ui
                            .add(egui::TextEdit::singleline(&mut port).desired_width(90.0))
                            .changed()
                        {
                            changed = Some(("port", port.clone()));
                        }
                        ui.label(
                            egui::RichText::new(trf!("服务地址 {}", inst.url(&host)))
                                .size(11.5)
                                .color(theme::dim()),
                        );
                    });
                    ui.end_row();

                    ui.label(egui::RichText::new("Profile").size(12.5));
                    ui.horizontal(|ui| {
                        let (c, v) = profile_combo(ui, "inst-profile", &profile, 150.0);
                        if c {
                            changed = Some(("profile", v.trim().to_string()));
                        }
                    });
                    ui.end_row();

                    // 版本在这里**只显示已选好的那一份**：选版本是「版本」分段的事，
                    // 两处都能改会让人不知道哪个说了算（这里也放不下版本列表）。
                    // 状态卡上那句「改版本 →」已经指了路，这一格不再重复说明。
                    ui.label(egui::RichText::new(tr!("DSH 版本")).size(12.5));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(version_text(
                                inst.configured_version(),
                                global_ver.as_deref(),
                            ))
                            .size(13.0)
                            .strong()
                            .color(if inst.cfg.version.trim().is_empty() {
                                theme::dim()
                            } else {
                                theme::ok()
                            }),
                        );
                    });
                    ui.end_row();

                    ui.label(egui::RichText::new(tr!("DSH 主目录")).size(12.5));
                    ui.vertical(|ui| {
                        if is_global {
                            // 全局实例的定义就是"用默认安装目录"：不给开关也不给改
                            ui.label(
                                egui::RichText::new(inst.home().display().to_string())
                                    .size(12.5)
                                    .color(theme::text()),
                            );
                            ui.label(
                                egui::RichText::new(tr!("全局实例固定使用默认 DSH 主目录"))
                                    .size(11.5)
                                    .color(theme::dim()),
                            );
                        } else {
                            if ui
                                .checkbox(
                                    &mut own_home,
                                    tr!("给这个实例单独一份 DSH_HOME"),
                                )
                                .on_hover_text(tr!(
                                    "开：这个实例有自己的 profile 与插件，多实例互不干扰。关：与其它实例共用 ~/.dsh"
                                ))
                                .changed()
                            {
                                changed =
                                    Some(("own_home", if own_home { "1" } else { "0" }.to_string()));
                            }
                            let shown = if home.trim().is_empty() {
                                inst.home().display().to_string()
                            } else {
                                home.clone()
                            };
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut home)
                                        .desired_width(420.0)
                                        .hint_text(shown),
                                )
                                .changed()
                            {
                                changed = Some(("home", home.clone()));
                            }
                        }
                    });
                    ui.end_row();
                });

            if let Some((f, v)) = changed {
                self.pending_inst_act = Some((idx, InstanceAct::EditField(f, v)));
            }

            ui.add_space(10.0);
            if is_global {
                // 全局实例不可删除：删了就没有"全局安装那一份"的对照物了
                ui.label(
                    egui::RichText::new(tr!(
                        "全局实例固定存在，不能删除；它用全局安装的 DSH 与默认主目录，与其它实例完全隔离。"
                    ))
                    .size(12.0)
                    .color(theme::dim()),
                );
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .button(tr!("删除这个实例"))
                        .on_hover_text(tr!(
                            "会先关掉它，只结束本启动器拉起的那棵树。它的 DSH 主目录会保留，不影响全局实例与其它实例"
                        ))
                        .clicked()
                    {
                        self.pending_inst_act = Some((idx, InstanceAct::Remove));
                    }
                });
            }
        });

        ui.add_space(12.0);

        // —— 日志 ——
        let log_path = inst.log_path();
        let inst_name = inst.cfg.name.clone();
        card(ui, |ui| {
            egui::CollapsingHeader::new(
                egui::RichText::new(trf!("{} 的运行日志", inst_name))
                    .size(15.0)
                    .color(theme::text()),
            )
            .default_open(false)
            .show(ui, |ui| {
                // 日志区**可滚动**：滚轮或右侧滚动条都能翻历史行。
                // 文本框按内容行数撑高，滚动交给 ScrollArea（否则会和文本框自身
                // 的内部滚动打架，滚轮就不好使了）。
                let tail = dsh::log_tail_at(&log_path, 400);
                if tail.trim().is_empty() {
                    ui.label(
                        egui::RichText::new(tr!("（这个实例还没有输出——先点「启动」）"))
                            .size(12.5)
                            .color(theme::dim()),
                    );
                } else {
                    log_view(ui, &tail, 280.0);
                }

                if let Some(out) = &self.last_output {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(tr!("最近操作输出")).size(13.5).color(theme::dim()));
                    log_view(ui, out, 180.0);
                }
            });
        });
    }

    /// 执行实例控制台里记下的动作。
    fn run_instance_act(&mut self, idx: usize, act: InstanceAct, ctx: &egui::Context) {
        // 这个 idx 是本帧早些时候记下的，执行时列表可能已经变了（比如刚删了一个）。
        // 越界就丢掉这个动作——GUI 里 panic 等于整个窗口消失，代价太大。
        if idx >= self.insts.len() {
            config::log(&format!(
                "dropping instance action: index {} out of range (len {})",
                idx,
                self.insts.len()
            ));
            return;
        }
        match act {
            InstanceAct::Start => self.start_instance(idx, ctx),
            InstanceAct::Stop => self.stop_instance(idx),
            InstanceAct::OpenUi => self.open_instance_ui(idx),
            // 删除会连磁盘上的 DSH_HOME 与已装版本一起清掉，不可逆——
            // 所以先弹确认，不直接删（见 confirm_bar 里 ConfirmAction::RemoveInstance）
            InstanceAct::Remove => {
                let c = self.insts.get(idx).map(|i| i.inst.cfg.clone());
                if let Some(cfg) = c {
                    self.pending_confirm = Some(ConfirmAction::RemoveInstance {
                        id: cfg.id,
                        name: cfg.name,
                    });
                }
            }
            // 概览里的「⬇ 安装」和版本页的「装到自管目录」都是"我要它跑这个版本"，
            // 所以装完自动切过去（then_use = true）
            InstanceAct::InstallVersion(v) => self.install_version_for(idx, ctx, v, true),
            InstanceAct::SetVersion(v) => {
                self.insts[idx].inst.cfg.version = v.clone();
                self.persist_instances();
                self.toast = Some((trf!("已改用自管版本 {}", v), false));
            }
            InstanceAct::EditField(field, value) => {
                // 全局实例的定义性字段不许改（UI 上也没给入口，这里是兜底）：
                // 版本必须是空（= 全局安装）、主目录必须是默认那份、名字固定
                if self.insts[idx].inst.cfg.is_global()
                    && matches!(field, "version" | "home" | "own_home" | "name")
                {
                    return;
                }
                let cfg = &mut self.insts[idx].inst.cfg;
                match field {
                    "name" => {
                        let v = value.trim();
                        if !v.is_empty() {
                            cfg.name = v.to_string();
                        }
                    }
                    "port" => {
                        // 端口解析失败就原样留着（不要让输入框里的半截数字把端口改坏）
                        if let Ok(p) = value.trim().parse::<u16>() {
                            if p >= 1024 {
                                cfg.port = p;
                            }
                        }
                    }
                    "profile" => cfg.profile = value.trim().to_string(),
                    "version" => cfg.version = value.trim().to_string(),
                    "home" => cfg.home = value.trim().to_string(),
                    "own_home" => cfg.own_home = value == "1",
                    _ => {}
                }
                self.persist_instances();
            }
        }
    }

    // ------------------------------------------------------ 版本（实例控制台分段）

    /// 版本管理（实例控制台的一个分段）。
    ///
    /// 两件事：全局安装那一份（切它 = 所有"用全局安装"的实例都跟着变），
    /// 以及自管版本（只给指定了版本的实例用）。
    /// 版本管理（实例控制台的一个分段）。
    ///
    /// **全局实例与自建实例管的是两件不同的事**，所以这一页分成两套：
    ///
    /// * 全局实例 → 只管**全局安装**那一份（切到哪个全局版本）
    /// * 自建实例 → 只管**这个实例**用自管目录里的哪一份
    ///
    /// 混在一起（以前那样）会让人以为"切换"和"装给这个实例"是同一件事的两个选项，
    /// 实际一个动全局、一个只动当前实例。
    fn console_versions(&mut self, ui: &mut egui::Ui) {
        // 每次进这一页都重扫一次**选中实例自己**的版本目录：只读列目录（毫秒级），
        // 换来的是"上次装完但没触发刷新"也能立刻出现在选择器里——否则会一直缺项，看着像坏了。
        let idx = self.sel();
        if let Some(i) = self.insts.get(idx) {
            self.managed_versions = dsh::managed_versions(&i.inst.cfg.id);
        }
        let picked_global = self.insts.get(idx).map(|i| i.inst.cfg.is_global()).unwrap_or(false);

        // 页头：标题 + 刷新 / 源码仓库
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(if picked_global {
                    tr!("全局版本")
                } else {
                    tr!("版本管理")
                })
                .size(15.0)
                .strong()
                .color(theme::text()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr!("刷新")).clicked() {
                    self.refresh_versions(ui.ctx());
                }
                ui.add_space(6.0);
                if ui.button(tr!("源码仓库")).clicked() {
                    if let Err(e) = webui::open_url(config::DSH_REPO) {
                        self.toast = Some((e, true));
                    }
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(tr!(
                "自动从 npm 检索 @deepseek-ai/dsh 的全部版本（与 GitHub 源码仓库同一来源）。"
            ))
            .size(13.0)
            .color(theme::dim()),
        );
        ui.add_space(10.0);

        if self.versions.versions.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(tr!("正在获取版本列表…")).size(13.0).color(theme::dim()));
            });
            // 列表还没回来，但"当前用的是哪一份"已经能显示
            self.versions_current_card(ui, idx, picked_global);
            return;
        }

        if picked_global {
            self.versions_page_global(ui);
        } else {
            self.versions_page_instance(ui, idx);
        }
    }

    /// 一行"当前用的是哪一份"。两个页面共用。
    fn versions_current_card(&mut self, ui: &mut egui::Ui, idx: usize, picked_global: bool) {
        let global_ver = self.installed.global.clone();
        let name = self
            .insts
            .get(idx)
            .map(|i| i.inst.cfg.name.clone())
            .unwrap_or_default();
        let cur = self
            .insts
            .get(idx)
            .map(|i| version_text(i.inst.configured_version(), global_ver.as_deref()))
            .unwrap_or_default();
        let is_ok = if picked_global {
            global_ver.is_some()
        } else {
            !self.selected_version().unwrap_or_default().trim().is_empty()
        };
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(if picked_global {
                        tr!("全局安装现在用的是").to_string()
                    } else {
                        trf!("「{}」用哪一份", name)
                    })
                    .size(15.0)
                    .strong()
                    .color(theme::text()),
                );
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(cur)
                        .size(14.0)
                        .strong()
                        .color(if is_ok { theme::ok() } else { theme::warn() }),
                );
            });
        });
    }

    /// 全局实例的版本页：**只管全局安装那一份**。
    fn versions_page_global(&mut self, ui: &mut egui::Ui) {
        self.versions_current_card(ui, self.sel(), true);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr!(
                "全局实例固定使用全局安装的那一份 DSH，不能改版本。这里是全局安装的管理页：\
                 点「切换」就换掉全局那份（命令行 dsh、以及自检也跟着变）。\
                 想让某个实例跑别的版本，去那个实例的「版本」页装——不会影响这里。"
            ))
            .size(12.0)
            .color(theme::dim()),
        );
        ui.add_space(10.0);

        // 全局安装现状
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr!("全局安装")).size(14.0).color(theme::dim()));
                ui.add_space(8.0);
                let cur = self.installed.global.clone().unwrap_or_else(|| tr!("（未检测到）").to_string());
                let col = if self.installed.global.is_some() { theme::ok() } else { theme::warn() };
                ui.label(egui::RichText::new(cur).size(14.0).strong().color(col));
            });
            if self.installed.global.is_none() {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        tr!("还没检测到全局安装。可以在下面选一个版本点「切换」，或手动执行 npm i -g @deepseek-ai/dsh。"),
                    )
                    .size(12.0)
                    .color(theme::warn()),
                );
            }
            if self.insts.iter().any(|i| i.inst.proc.is_some()) {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        tr!("注意：有 DSH 正在运行（由本启动器启动）。切换版本会覆盖全局安装里的文件，Windows 上可能因文件占用失败——建议先关掉它。"),
                    )
                    .size(12.0)
                    .color(theme::warn()),
                );
            }
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr!("切换到哪个全局版本"))
                    .size(14.0)
                    .strong()
                    .color(theme::text()),
            );
            if let Some(t) = &self.installing {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(
                    egui::RichText::new(trf!("正在把全局安装切换为 {} …", t))
                        .size(12.5)
                        .color(theme::warn()),
                );
            }
            if self.uninstalling_global {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(
                    egui::RichText::new(tr!("正在卸载全局安装…"))
                        .size(12.5)
                        .color(theme::warn()),
                );
            }
        });
        ui.add_space(6.0);

        let current = self.installed.global.clone();
        let latest = self.versions.latest.clone();
        let list = self.versions.versions.clone();
        // 全局安装正在被哪些实例使用（切换前让他知道会影响谁）
        let users: Vec<String> = self
            .insts
            .iter()
            .filter(|i| i.inst.cfg.is_global())
            .map(|i| i.inst.cfg.name.clone())
            .collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for v in list.iter() {
                let active = current.as_deref() == Some(v.version.as_str());
                // 在当前版本上给「卸载」（原来这里是「重新装」——那和「切换」是同一个动作，
                // 白花一两分钟还可能因为文件被占用失败，没有意义）；
                // 其它版本给「切换」。
                let (primary, secondary) = if active {
                    (
                        None,
                        Some((
                            tr!("卸载").to_string(),
                            tr!("卸载全局安装：卸载后全局实例、命令行 dsh 与自检都没得跑，直到重新装一个")
                                .to_string(),
                        )),
                    )
                } else {
                    (
                        Some((
                            tr!("切换").to_string(),
                            trf!("把全局安装换成 {}（{} 会跟着变）", v.version, users.join("、")),
                        )),
                        None,
                    )
                };
                match version_row(
                    ui,
                    v,
                    &latest,
                    if active { Some(tr!("全局安装在用")) } else { None },
                    primary,
                    secondary,
                ) {
                    VersionRowAct::Primary => self.install_version(ui.ctx(), v.version.clone()),
                    VersionRowAct::Secondary => {
                        self.pending_confirm = Some(ConfirmAction::UninstallGlobal)
                    }
                    VersionRowAct::None => {}
                }
                ui.add_space(4.0);
            }
        });
    }

    /// 自建实例的版本页：**只管这个实例用自管目录里的哪一份**。
    fn versions_page_instance(&mut self, ui: &mut egui::Ui, idx: usize) {
        let wanted = self.selected_version().unwrap_or_default().trim().to_string();
        let inst_name = self
            .insts
            .get(idx)
            .map(|i| i.inst.cfg.name.clone())
            .unwrap_or_default();
        // 卸载要按 id 走（版本列表可能在确认框弹出后被改）
        let inst_id = self
            .insts
            .get(idx)
            .map(|i| i.inst.cfg.id.clone())
            .unwrap_or_default();

        self.versions_current_card(ui, idx, false);
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(tr!(
                "这个实例只用自管目录里的版本；换了只影响它自己，不动全局安装、也不动别的实例。"
            ))
            .size(12.0)
            .color(theme::dim()),
        );
        ui.add_space(10.0);

        let latest = self.versions.latest.clone();
        let list = self.versions.versions.clone();
        let managed_here = self.managed_versions.clone();
        let ready: Vec<_> = list
            .iter()
            .filter(|v| managed_here.iter().any(|m| m == &v.version))
            .collect();
        let installable: Vec<_> = list
            .iter()
            .filter(|v| !managed_here.iter().any(|m| m == &v.version))
            .collect();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // —— 已装到自管目录：点一下就用它 ——
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(tr!("已装到自管目录（点一下就用它）"))
                        .size(14.0)
                        .strong()
                        .color(theme::ok()),
                );
                if let Some(t) = &self.installing_managed {
                    ui.add_space(8.0);
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(trf!("正在装 {} …", t)).size(12.5).color(theme::warn()),
                    );
                }
            });
            ui.add_space(6.0);
            if ready.is_empty() {
                ui.label(
                    egui::RichText::new(tr!("（这个实例还没有可用的版本——从下面装一个）"))
                        .size(12.0)
                        .color(theme::warn()),
                );
            } else {
                for v in &ready {
                    let selected = wanted == v.version;
                    // 正在用的那个：主按钮不给（它就是当前选择），但**允许卸载**——
                    // 卸载后这个实例会回到"没版本"状态，界面会引导去装一个。
                    let primary = if selected {
                        None
                    } else {
                        Some((
                            tr!("使用").to_string(),
                            trf!("让「{}」改用 {}", inst_name, v.version),
                        ))
                    };
                    let secondary = Some((
                        tr!("卸载").to_string(),
                        trf!(
                            "把这个版本从「{}」删掉（约几百 MB），不碰它的 DSH 主目录、也不碰别的实例",
                            inst_name
                        ),
                    ));
                    match version_row(
                        ui,
                        v,
                        &latest,
                        if selected { Some(tr!("该实例在用")) } else { None },
                        primary,
                        secondary,
                    ) {
                        VersionRowAct::Primary => {
                            self.pending_inst_act =
                                Some((idx, InstanceAct::SetVersion(v.version.clone())))
                        }
                        VersionRowAct::Secondary => {
                            self.pending_confirm = Some(ConfirmAction::UninstallInstanceVersion {
                                id: inst_id.clone(),
                                name: inst_name.clone(),
                                version: v.version.clone(),
                            })
                        }
                        VersionRowAct::None => {}
                    }
                    ui.add_space(4.0);
                }
            }

            // —— 还没装的：装给这个实例 ——
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(tr!("安装新版本（装完自动切过去并启动）"))
                    .size(14.0)
                    .strong()
                    .color(theme::text()),
            );
            ui.add_space(6.0);
            for v in &installable {
                // 按钮就叫「安装」：这一页已经写明了是给哪个实例装的
                // （标题「甲」用哪一份、分组标题也说了装给这个实例），
                // 再把实例名塞进按钮里只是啰嗦。
                let primary = Some((
                    tr!("安装").to_string(),
                    trf!("把 {} 装到自管目录并让「{}」用它（不碰全局安装）", v.version, inst_name),
                ));
                if version_row(ui, v, &latest, None, primary, None) == VersionRowAct::Primary {
                    self.pending_inst_act =
                        Some((idx, InstanceAct::InstallVersion(v.version.clone())));
                }
                ui.add_space(4.0);
            }
        });
    }

    // ------------------------------------------------------ 插件（实例控制台分段）

    /// 插件管理（实例控制台的一个分段）。操作的都是**选中实例**的 profile 与 DSH_HOME。
    fn console_plugins(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(tr!("插件管理"))
                    .size(15.0)
                    .strong()
                    .color(theme::text()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // 刷新：本地重扫（瞬时）+ 后台重查最新版本 + 后台重抓市场（旧列表先留着）
                if ui
                    .button(egui::RichText::new(tr!("⟳  刷新")).size(14.0))
                    .on_hover_text(
                        tr!("重新扫描已装插件（profile 依赖 / bundle 层 / node_modules / pnpm 存储 / \
                         兜底目录 / 共享目录 / dsh 自带）、重新检查它们的最新版本，\
                         并重新抓取插件市场"),
                    )
                    .clicked()
                {
                    self.rescan_plugins();
                    // 重扫之后包目录/版本可能变了，最新版本也要重查一遍：
                    // 查到新版就点亮「更新」，开着"自动更新"的话会顺带自动装掉
                    self.check_plugin_updates(ui.ctx());
                    self.reload_market(ui.ctx());
                }
                if self.market_loading {
                    ui.spinner();
                } else if self.market_loaded {
                    ui.label(
                        egui::RichText::new(trf!("市场共 {} 个插件", self.market.len()))
                            .size(12.5)
                            .color(theme::dim()),
                    );
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(trf!("搜索并管理 DeepSeek Harness 插件（profile: {}）。启停写入 cordis.patch.yml。", self.profile()))
            .size(13.0)
            .color(theme::dim()),
        );
        ui.add_space(4.0);

        // 本次扫描的途径汇总：让"这些插件是怎么进来的"一眼可见
        let age = self
            .plugin_scan
            .scanned_at
            .map(|t| t.elapsed().as_secs())
            .map(|s| if s < 5 { tr!("刚刚").to_string() } else { trf!("{} 秒前", s) })
            .unwrap_or_else(|| tr!("尚未扫描").to_string());
        let roots_hover = trf!("插件是从这些地方找出来的：\n{}", if self.plugin_scan.roots.is_empty() {
                "（没有可查的目录）".to_string()
            } else {
                self.plugin_scan.roots.join("\n")
            });
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(trf!("扫描途径（profile {}，{} 刷新）：", self.plugin_scan.profile, age))
                    .size(12.0)
                    .color(theme::dim()),
            )
            .on_hover_text(&roots_hover);
            let hit: Vec<(plugins::PluginRoute, usize)> = self
                .plugin_scan
                .counts
                .iter()
                .filter(|(_, n)| *n > 0)
                .cloned()
                .collect();
            if hit.is_empty() {
                ui.label(
                    egui::RichText::new(tr!("（一条途径都没扫到东西）"))
                        .size(11.5)
                        .color(theme::warn()),
                );
            }
            for (route, n) in hit {
                ui.label(
                    egui::RichText::new(format!("{} {}", route.label(), n))
                        .size(11.5)
                        .color(theme::accent()),
                )
                .on_hover_text(route.hint());
            }
        });
        ui.add_space(10.0);

        // —— 待确认的动作（更新插件）：装包之前先问一句 ——
        self.confirm_bar(ui);

        // 搜索行
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.market_query)
                    .hint_text(tr!("搜索插件名称 / 作者 / 描述…"))
                    .desired_width(300.0),
            );
            if ui.button(tr!("搜索 GitHub")).clicked() {
                let q = self.market_query.clone();
                self.github_loading = true;
                self.tasks.spawn(ui.ctx(), move || {
                    TaskResult::Github(plugins::search_github(&q))
                });
            }
            if self.github_loading {
                ui.spinner();
            }
        });
        ui.add_space(8.0);

        // 分类：内部值仍是市场里的英文键（过滤按它比较），界面显示中文
        let cats = [
            "全部", "ui", "theme", "fun", "tools", "model", "usage", "session", "memory",
            "notify", "workflow", "git", "docs", "voice", "vision", "security", "market", "github",
        ];
        ui.horizontal_wrapped(|ui| {
            for c in cats {
                let selected = self.market_category == c;
                let label = if c == "全部" {
                    tr!("全部").to_string()
                } else {
                    plugins::category_label(c)
                };
                if ui.selectable_label(selected, label).clicked() {
                    self.market_category = c.to_string();
                }
            }
        });
        ui.add_space(10.0);

        if let Some(b) = &self.plugin_busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(b).size(13.0).color(theme::warn()));
            });
            ui.add_space(8.0);
        }

        // —— 已安装插件（用户装的，可能来自多条途径）——
        let scan = self.plugin_scan.clone();
        if scan.plugins.is_empty() {
            ui.label(
                egui::RichText::new(tr!("没有扫描到已安装的插件。"))
                    .size(13.0)
                    .color(theme::dim()),
            );
            ui.add_space(8.0);
        } else {
            egui::CollapsingHeader::new(
                egui::RichText::new(trf!("已安装插件（{}）", scan.plugins.len()))
                    .size(14.5)
                    .strong()
                    .color(theme::text()),
            )
            .default_open(true)
            .show(ui, |ui| {
                let ctx = ui.ctx().clone();
                let mut actions = Vec::new();
                // 固定"一屏 4 行"：多的靠滚轮或拖右侧滚动条气泡翻（见 scroll_rows）。
                // 行高是上一帧实测出来的，所以换个字号/DPI 也不会把第 4 行截掉。
                self.plugin_row_h = scroll_rows(
                    ui,
                    self.plugin_row_h,
                    scan.plugins.len(),
                    |ui| {
                        for p in scan.plugins.iter() {
                            // 只有确实比装的新的版本才当"可更新"
                            let latest = self
                                .plugin_updates
                                .get(&p.package)
                                .map(|s| s.as_str())
                                .filter(|l| plugins::is_newer(l, &p.version));
                            actions.push(installed_row(ui, p, latest));
                        }
                    },
                );
                for a in actions {
                    self.run_installed_action(&ctx, a);
                }
            });
            ui.add_space(10.0);
        }

        // 注意：「dsh 安装自带」的平台层（dsh-base / dsh-web-app 这些）**不在插件页显示**——
        // 它们不是用户装的插件，扫描时仍然识别（途径汇总里会显示条数），只是不列出来。

        // —— 扫描提示 ——
        if !scan.warnings.is_empty() {
            egui::CollapsingHeader::new(
                egui::RichText::new(trf!("扫描提示（{}）", scan.warnings.len()))
                    .size(13.5)
                    .color(theme::warn()),
            )
            .default_open(false)
            .show(ui, |ui| {
                for w in scan.warnings.iter() {
                    ui.label(egui::RichText::new(format!("· {}", w)).size(12.0).color(theme::dim()));
                }
            });
            ui.add_space(10.0);
        }

        // —— GitHub 搜索结果 ——
        if !self.github_results.is_empty() {
            ui.label(
                egui::RichText::new(trf!("GitHub 搜索结果（{}）", self.github_results.len()))
                    .size(14.0)
                    .strong()
                    .color(theme::text()),
            );
            ui.add_space(6.0);
            let list = self.github_results.clone();
            let ctx = ui.ctx().clone();
            let mut actions = Vec::new();
            for p in list.iter() {
                actions.push(plugin_row(ui, p));
            }
            for a in actions {
                handle_row_action(self, a, &ctx);
            }
            ui.add_space(10.0);
        }

        // —— 市场列表 ——
        if !self.market_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new(tr!("正在加载插件市场…")).size(14.0).color(theme::dim()),
                );
                if ui.button(tr!("重新加载")).clicked() {
                    self.reload_market(ui.ctx());
                }
            });
            return;
        }

        let filtered =
            plugins::filter_local(&self.market, &self.market_query, &self.market_category);
        ui.label(
            egui::RichText::new(trf!("插件市场（{} 个结果）", filtered.len()))
                .size(14.0)
                .strong()
                .color(theme::text()),
        );
        ui.add_space(6.0);

        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for p in filtered.iter().take(300) {
                actions.push(plugin_row(ui, p));
            }
        });
        for a in actions {
            handle_row_action(self, a, &ctx);
        }
    }

    // ------------------------------------------------------ 启动器设置

    /// 启动器自己的设置：**与具体实例无关**的那些。
    ///
    /// 语言、风格、关闭行为、自动开浏览器、插件更新策略——这些是"程序怎么表现"，
    /// 全程序一份。实例相关的（端口 / profile / 版本 / 插件）都在实例控制台里。
    fn tab_launcher_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new(tr!("设置")).size(21.0).color(theme::text()));
        ui.add_space(12.0);

        // 设置项比窗口高（尤其小窗口），套一层滚动区：滚轮 / 拖气泡都能翻到底部卡片
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut changed = false;

            // 语言：中文 / English（切换立即生效并写进设置）
            // 标题写成"语言/Language"这种双语形式：它本身就是语言设置，
            // 翻译成单一语言反而让人找不着。
            card(ui, |ui| {
                ui.label(
                    egui::RichText::new("语言/Language")
                        .size(14.0)
                        .strong()
                        .color(theme::text()),
                );
                ui.add_space(8.0);
                let mut language = self.settings.language;
                let before = language;
                egui::ComboBox::from_id_salt("language")
                    .selected_text(language.label())
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut language, Lang::Zh, Lang::Zh.label());
                        ui.selectable_value(&mut language, Lang::En, Lang::En.label());
                    });
                if language != before {
                    self.settings.language = language;
                    i18n::set(language);
                    changed = true;
                    ui.ctx().request_repaint();
                }
            });

            ui.add_space(10.0);

            // 风格：浅色 / 深色（切换立即生效，并写进设置）
            card(ui, |ui| {
                ui.label(egui::RichText::new(tr!("风格")).size(14.0).strong().color(theme::text()));
                ui.add_space(8.0);
                let mut dark = self.settings.theme == ThemeMode::Dark;
                let before = dark;
                ui.radio_value(&mut dark, false, tr!("浅色"));
                ui.radio_value(&mut dark, true, tr!("深色"));
                if dark != before {
                    self.settings.theme = if dark { ThemeMode::Dark } else { ThemeMode::Light };
                    theme::set_dark(dark);
                    apply_theme(ui.ctx());
                    ui.ctx().request_repaint();
                    changed = true;
                }
            });

            ui.add_space(10.0);

            // 关闭按钮行为（✕ 是收进托盘还是直接退出）
            card(ui, |ui| {
                ui.label(egui::RichText::new(tr!("关闭按钮行为")).size(14.0).strong().color(theme::text()));
                ui.add_space(8.0);
                let mut to_tray = self.settings.close_action == CloseAction::Tray;
                let before = to_tray;
                ui.radio_value(&mut to_tray, true, tr!("最小化到系统托盘（后台继续运行）"));
                ui.radio_value(&mut to_tray, false, tr!("直接关闭（点 × 即刻退出）"));
                if to_tray != before {
                    self.settings.close_action =
                        if to_tray { CloseAction::Tray } else { CloseAction::Exit };
                    // 托盘右键菜单里那一项的勾要跟着变
                    tray::set_close_to_tray(to_tray);
                    changed = true;
                }
                if !self.tray_ok {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(tr!("当前平台不支持系统托盘，关闭按钮将直接退出。"))
                            .size(12.0)
                            .color(theme::warn()),
                    );
                }
            });

            ui.add_space(10.0);

            card(ui, |ui| {
                let mut auto = self.settings.auto_open_browser;
                if ui
                    .checkbox(&mut auto, tr!("启动 DSH 后自动在浏览器打开 Web UI"))
                    .changed()
                {
                    self.settings.auto_open_browser = auto;
                    changed = true;
                }
            });

            ui.add_space(10.0);

            // 插件更新策略：自动装 / 只检查、等确认
            card(ui, |ui| {
                ui.label(
                    egui::RichText::new(tr!("插件更新"))
                        .size(14.0)
                        .strong()
                        .color(theme::text()),
                );
                ui.add_space(8.0);
                let mut auto = self.settings.auto_update_plugins;
                let before = auto;
                ui.radio_value(&mut auto, false, tr!("只自动检查：发现新版本后在插件页标出来，确认后才更新"));
                ui.radio_value(&mut auto, true, tr!("自动更新：检查到新版本就直接更新到最新版"));
                if auto != before {
                    self.settings.auto_update_plugins = auto;
                    // 刚打开"自动更新"→ 允许本轮再自动跑一次
                    if auto {
                        self.auto_update_ran = false;
                    }
                    changed = true;
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        tr!("更新走的是 dsh 自己的插件命令：dsh plugin --profile <profile> add <包名>@latest（底层 pnpm）。"),
                    )
                    .size(12.0)
                    .color(theme::dim()),
                );
            });

            if changed {
                self.settings.save();
                self.rescan_plugins();
                // 改了插件更新策略：立刻重查一次（开着"自动更新"的话顺手把插件升掉）
                self.check_plugin_updates(ui.ctx());
                self.toast = Some((tr!("设置已保存").into(), false));
            }
        });
    }

    // ------------------------------------------------------ 提示条

    fn toast_bar(&mut self, ui: &mut egui::Ui) {
        let Some((msg, err)) = self.toast.clone() else { return };
        let mut open = true;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&msg)
                    .size(13.5)
                    .strong()
                    .color(if err { theme::err() } else { theme::ok() }),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("×").clicked() {
                    open = false;
                }
            });
        });
        if !open {
            self.toast = None;
        }
    }

    /// 真正退出：关掉所有自己拉起的实例、摘托盘。
    fn shutdown(&mut self) {
        config::log("shutting down all DSH instances");
        self.stop_all_instances();
        tray::remove();
        config::log("launcher finished");
    }
}

/// 只读日志视图：内容按行数撑高，外面套一层**可滚动**区域（滚轮 / 滚动条）。
fn log_view(ui: &mut egui::Ui, text: &str, max_height: f32) {
    let rows = text.lines().count().clamp(6, 600);
    let body = text.to_string();
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut body.as_str())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(rows),
            );
        });
}

// ------------------------------------------------------------------ 小部件

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(theme::card())
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(16))
        .stroke(egui::Stroke::new(1.0, theme::border()))
        .shadow(card_shadow())
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

/// 侧栏里的一项：可点、有主副标题，可选一个状态小圆点。
///
/// 返回是否被点击。抽出来是因为侧栏里现在有三类项（启动器设置 / 新建实例 / 每个实例），
/// 手绘一遍容易各画各样。
fn sidebar_item(
    ui: &mut egui::Ui,
    selected: bool,
    title: &str,
    hint: &str,
    dot: Option<egui::Color32>,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), egui::Sense::click());
    let hovered = resp.hovered();
    let bg = if selected {
        theme::accent_soft()
    } else if hovered {
        theme::surface()
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 10.0, bg);
    if selected {
        let bar =
            egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 11.0), egui::vec2(3.0, 28.0));
        ui.painter().rect_filled(bar, 2.0, theme::accent());
    }

    // 状态圆点占最左边一格；没有圆点时文字往前挪，保持左边距统一
    let text_x = if let Some(col) = dot {
        let c = rect.min + egui::vec2(20.0, 25.0);
        ui.painter().circle_filled(c, 4.0, col);
        30.0
    } else {
        16.0
    };

    let text_col = if selected { theme::accent() } else { theme::text() };
    ui.painter().text(
        rect.min + egui::vec2(text_x, 9.0),
        egui::Align2::LEFT_TOP,
        title,
        egui::FontId::proportional(14.5),
        text_col,
    );
    // 副标题可能很长（端口 + 版本 + 状态）：截断，完整内容放悬停里
    let hint_short = truncate(hint, 22);
    ui.painter().text(
        rect.min + egui::vec2(text_x, 28.0),
        egui::Align2::LEFT_TOP,
        &hint_short,
        egui::FontId::proportional(11.0),
        theme::dim(),
    );
    resp.on_hover_text(hint).clicked()
}

/// 状态药丸：淡色底 + 圆点 + 文字（顶栏用）。
fn status_chip(ui: &mut egui::Ui, label: &str, fg: egui::Color32, bg: egui::Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), egui::FontId::proportional(13.5), fg);
    let pad = egui::vec2(12.0, 6.0);
    let dot = 8.0;
    let size = egui::vec2(
        pad.x * 2.0 + dot + 7.0 + galley.size().x,
        (pad.y * 2.0 + galley.size().y).max(30.0),
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, egui::CornerRadius::same(15), bg);
    let cy = rect.center().y;
    p.circle_filled(egui::pos2(rect.left() + pad.x + dot / 2.0, cy), 4.0, fg);
    p.galley(
        egui::pos2(rect.left() + pad.x + dot + 7.0, cy - galley.size().y / 2.0),
        galley,
        fg,
    );
}

/// 已装插件一行里可发生的动作（绘制结束后统一处理，避免绘制期间可变借用 self）。
enum InstalledAction {
    None,
    Uninstall(String),
    Toggle {
        label: String,
        ids: Vec<String>,
        enabled: bool,
    },
    /// 更新到最新版（`latest` 是查到的最新版本号，可能没查到）
    Update {
        package: String,
        latest: Option<String>,
    },
    /// 在系统浏览器里打开这个插件的项目仓库
    OpenRepo(String),
}

/// 需要用户点一下"确认"才会执行的动作（写在配置里的改动，先问一句）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfirmAction {
    /// 把插件更新到最新版（`latest` 为空表示"更新到最新"，版本号未知）
    Update { package: String, latest: Option<String> },
    /// 删掉一个实例（含它磁盘上的 DSH_HOME 与已装版本）。按 id 记，避免下标错位。
    RemoveInstance { id: String, name: String },
    /// 卸载全局安装（所有自建实例都不受影响）。
    UninstallGlobal,
    /// 卸载某个实例自己装的某个版本（只删那一份，不动它的 DSH_HOME、不动别的实例）。
    UninstallInstanceVersion { id: String, name: String, version: String },
}

impl ConfirmAction {
    /// 确认条上的一句话。
    fn question(&self) -> String {
        match self {
            Self::Update { package, latest: Some(v) } => {
                trf!("把 {} 更新到最新版 {}？", package, v)
            }
            Self::Update { package, latest: None } => {
                trf!("把 {} 更新到最新版？", package)
            }
            // 说清楚"要删掉什么"、以及删在哪儿：不可逆的操作不能只写"确定删除？"
            Self::RemoveInstance { id, name } => trf!(
                "删除实例「{}」？会一并删掉 {}（它的 DSH 主目录与它自己装的 DSH 版本），不可撤销。",
                name,
                config::instance_dir(id).display()
            ),
            // 全局版本在界面渲染时已经查过一次（`installed`），这里直接用缓存，
            // 不在画确认框的时候再去读磁盘
            Self::UninstallGlobal => trf!(
                "卸载全局安装（当前 {}）？卸载后全局实例、命令行 dsh 与自检都没得跑，\
                 直到重新装一个；自建实例各自的版本不受影响。",
                versions::installed()
                    .global
                    .unwrap_or_else(|| tr!("（未检测到）").to_string())
            ),
            Self::UninstallInstanceVersion { name, version, .. } => trf!(
                "把 {} 从「{}」卸载？只删这一份（约几百 MB），不碰它的 DSH 主目录、也不影响别的实例。",
                version,
                name
            ),
        }
    }

    /// 确认按钮上的字。
    fn ok_label(&self) -> &'static str {
        match self {
            Self::Update { .. } => tr!("确认更新"),
            Self::RemoveInstance { .. } => tr!("删除"),
            Self::UninstallGlobal | Self::UninstallInstanceVersion { .. } => tr!("卸载"),
        }
    }

    /// 是不是"会丢数据"的动作（按钮用警示色，让人先看一眼再点）。
    fn is_destructive(&self) -> bool {
        !matches!(self, Self::Update { .. })
    }
}

/// 已安装插件一屏显示的行数：超出的靠滚轮 / 拖动滚动条气泡翻。
const PLUGIN_ROWS_VISIBLE: f32 = 4.0;
/// 首次绘制时先按这个行高算可视高度，之后用实测值（见 `scroll_rows` 的返回值）。
const PLUGIN_ROW_H_FALLBACK: f32 = 74.0;
/// 一行内容的最小高度（按钮行 + id 行）：矮了就把行撑到这个高度，行高才稳定。
const PLUGIN_ROW_CONTENT_H: f32 = 52.0;

/// 把一组插件行装进"固定 N 行高"的滚动区。
///
/// - **滚轮**：`ScrollArea` 自带；指针停在列表上时滚的是这个列表（不会带着整页跑）
/// - **拖动气泡**：滚动条**沿用全局默认样式**（egui 的悬浮气泡，和运行日志、插件市场
///   那几处一模一样），不在这里改 `spacing.scroll`——风格要统一；气泡本身可以按住拖
/// - 行数不足 N 行时按内容收缩，不占空位
///
/// 返回**实测的每行高度**（内容总高 ÷ 行数）：字号、DPI、字体回退变了也不会算错，
/// 调用方把它存下来，下一帧就能用"正好 4 行"的高度。
fn scroll_rows(
    ui: &mut egui::Ui,
    row_h: f32,
    count: usize,
    add: impl FnOnce(&mut egui::Ui),
) -> f32 {
    let out = egui::ScrollArea::vertical()
        .max_height(PLUGIN_ROWS_VISIBLE * row_h.max(1.0))
        .auto_shrink([false, true])
        .show(ui, add);
    if count > 0 && out.content_size.y > 0.0 {
        out.content_size.y / count as f32
    } else {
        row_h
    }
}

/// 途径标签的配色：登记过的用绿，需要留意的用黄。
fn route_color(r: PluginRoute) -> egui::Color32 {
    match r {
        PluginRoute::Dependency => theme::ok(),
        PluginRoute::BundleLayer | PluginRoute::SharedModules => theme::accent(),
        PluginRoute::Installation => theme::dim(),
        _ => theme::warn(),
    }
}

/// 已装插件的一行：包名 + 版本 + 途径标签 + id 行 + 更新/启停/卸载。
///
/// 行高被 `PLUGIN_ROW_CONTENT_H` 固定住（id 行截断成一行），否则字号或长包名一变，
/// "一屏正好 4 行"就算不准。
///
/// `latest`：后台查到的最新版本，且确实比装的新——有值时按钮变成「更新 <版本>」，
/// 版本号旁边也会显示 `已装 → 最新`。
fn installed_row(
    ui: &mut egui::Ui,
    p: &InstalledPlugin,
    latest: Option<&str>,
) -> InstalledAction {
    let mut action = InstalledAction::None;
    egui::Frame::NONE
        .fill(theme::card())
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .stroke(egui::Stroke::new(1.0, theme::border()))
        .show(ui, |ui| {
            // 行内两行文字挨紧一点，行高才好预测
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.set_min_height(PLUGIN_ROW_CONTENT_H);
            ui.horizontal(|ui| {
                let name = ui.label(
                    egui::RichText::new(&p.package)
                        .size(14.0)
                        .strong()
                        .color(if p.enabled { theme::text() } else { theme::dim() }),
                );
                if let Some(dir) = &p.dir {
                    name.on_hover_text(dir.display().to_string());
                }
                match (p.version.is_empty(), latest) {
                    (false, Some(v)) => {
                        ui.label(
                            egui::RichText::new(format!("{} → {}", p.version, v))
                                .size(12.0)
                                .strong()
                                .color(theme::ok()),
                        )
                        .on_hover_text(tr!("已装版本 → npm 上的最新版本"));
                    }
                    (false, None) => {
                        ui.label(egui::RichText::new(&p.version).size(12.0).color(theme::dim()));
                    }
                    _ => {}
                }
                // 途径标签：最多显示两个，其余折成 "+N"，悬停看完整解释
                for r in p.routes.iter().take(2) {
                    ui.label(egui::RichText::new(r.label()).size(11.5).color(route_color(*r)))
                        .on_hover_text(r.hint());
                }
                if p.routes.len() > 2 {
                    ui.label(
                        egui::RichText::new(format!("+{}", p.routes.len() - 2))
                            .size(11.0)
                            .color(theme::dim()),
                    )
                    .on_hover_text(p.route_detail());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // 自带层不给"卸载"：那是 dsh 安装自己的依赖，删了会把平台拆坏
                    let removable = p.origin == PluginOrigin::Profile;
                    if removable && ui.button(tr!("卸载")).clicked() {
                        action = InstalledAction::Uninstall(p.package.clone());
                    }
                    if !p.ids.is_empty() {
                        let label = if p.enabled { tr!("禁用") } else { tr!("启用") };
                        if ui.button(label).clicked() {
                            action = InstalledAction::Toggle {
                                label: p.package.clone(),
                                ids: p.ids.clone(),
                                enabled: !p.enabled,
                            };
                        }
                    }
                    // 更新按钮：查到新版才可点，否则置灰并说明原因
                    let (enabled_btn, hint) = match latest {
                        Some(v) => (true, trf!("更新到 {}", v)),
                        None if p.version.is_empty() => {
                            (false, tr!("读不到已装版本，无法判断是否有新版").to_string())
                        }
                        None => (
                            false,
                            tr!("已是最新，或这个包不是从 npm 装的（GitHub / 本地路径）——查不到新版")
                                .to_string(),
                        ),
                    };
                    let label = match latest {
                        Some(v) => trf!("更新 {}", v),
                        None => tr!("更新").to_string(),
                    };
                    let btn = egui::Button::new(
                        egui::RichText::new(label)
                            .size(14.0)
                            .color(if enabled_btn { egui::Color32::WHITE } else { theme::dim() }),
                    )
                    .fill(if enabled_btn { theme::accent_solid() } else { theme::surface() })
                    .stroke(egui::Stroke::NONE);
                    if ui
                        .add_enabled(enabled_btn, btn)
                        .on_hover_text(hint.clone())
                        .on_disabled_hover_text(hint)
                        .clicked()
                    {
                        action = InstalledAction::Update {
                            package: p.package.clone(),
                            latest: latest.map(|s| s.to_string()),
                        };
                    }
                    // 仓库按钮：地址来自包自己的 package.json（npm 包回落 npm 页面）；
                    // 取不到地址（GitHub / 本地路径装的）就不给按钮，免得点了打不开
                    if let Some(repo) = &p.repo {
                        if ui
                            .button(tr!("仓库"))
                            .on_hover_text(trf!("在浏览器打开 {} 的项目仓库：\n{}", p.package, repo))
                            .clicked()
                        {
                            action = InstalledAction::OpenRepo(repo.clone());
                        }
                    }
                });
            });
            let warn = p.ids.is_empty();
            // 截断成一行（完整内容在悬停里）：id 多的时候换行会把行高顶开
            let line = ui.add(
                egui::Label::new(
                    egui::RichText::new(p.id_line())
                        .size(12.5)
                        .color(if warn { theme::warn() } else { theme::dim() }),
                )
                .truncate(),
            );
            line.on_hover_text(match &p.dir {
                Some(dir) => format!("{}\n{}", p.id_line(), dir.display()),
                None => p.id_line(),
            });
        });
    ui.add_space(4.0);
    action
}

/// 插件市场一行里可发生的动作。
enum RowAction {
    None,
    Install(PluginInfo),
    OpenRepo(String),
}

/// 插件市场的一行。返回用户动作，由调用方在绘制结束后处理
/// （避免在绘制期间同时可变借用 self 与 ui）。
fn plugin_row(ui: &mut egui::Ui, p: &PluginInfo) -> RowAction {
    let mut action = RowAction::None;
    egui::Frame::NONE
        .fill(theme::card())
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .stroke(egui::Stroke::new(1.0, theme::border()))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    // 右侧要放「仓库 / 安装」两个按钮，先给它们留出宽度，
                    // 否则描述文字会被按钮压住
                    let reserve = if p.url.is_empty() { 72.0 } else { 132.0 };
                    ui.set_max_width((ui.available_width() - reserve).max(160.0));
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&p.name).size(14.0).strong().color(theme::text()),
                        );
                        if !p.owner.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("@{}", p.owner))
                                    .size(12.0)
                                    .color(theme::dim()),
                            );
                        }
                        if !p.category.is_empty() {
                            ui.label(
                                egui::RichText::new(plugins::category_label(&p.category))
                                    .size(11.5)
                                    .color(theme::accent()),
                            );
                        }
                        if p.stars > 0 {
                            ui.label(
                                egui::RichText::new(format!("★ {}", p.stars))
                                    .size(11.5)
                                    .color(theme::warn()),
                            );
                        }
                        if !p.npm.is_empty() {
                            ui.label(egui::RichText::new("npm").size(11.5).color(theme::ok()));
                        }
                    });
                    let desc = if !p.description_zh.is_empty() {
                        p.description_zh.clone()
                    } else {
                        p.description.clone()
                    };
                    if !desc.is_empty() {
                        ui.label(
                            egui::RichText::new(truncate(&desc, 140)).size(12.5).color(theme::dim()),
                        );
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr!("安装")).clicked() {
                        action = RowAction::Install(p.clone());
                    }
                    if !p.url.is_empty() && ui.button(tr!("仓库")).clicked() {
                        action = RowAction::OpenRepo(p.url.clone());
                    }
                });
            });
        });
    ui.add_space(4.0);
    action
}

/// 处理插件行的动作。
fn handle_row_action(app: &mut App, act: RowAction, ctx: &egui::Context) {
    match act {
        RowAction::None => {}
        RowAction::Install(p) => app.install_plugin(ctx, p),
        RowAction::OpenRepo(url) => {
            if let Err(e) = webui::open_url(&url) {
                app.toast = Some((e, true));
            }
        }
    }
}

/// 实例控制台里能发生的动作。
///
/// 由各个分段（概览 / 版本 / 插件）在绘制时**记下来**，绘制结束后统一执行——
/// 绘制期间已经可变借用了 `self`，当场改会打架。
enum InstanceAct {
    Start,
    Stop,
    OpenUi,
    Remove,
    /// 把这个版本装到自管目录
    InstallVersion(String),
    /// 让这个实例改用某个自管版本
    SetVersion(String),
    /// 改配置里的某一项：(字段名, 新值)
    EditField(&'static str, String),
}

/// 可输入的 profile 下拉框：**预设选项 + 自定义**。
///
/// 输入框可以直接手输任意 profile 名；下拉框里列的是常用预设。
/// 下拉框显示"当前落在哪个预设上"，不是预设就显示「自定义」——
/// 选中「自定义」会把输入框清空，直接开始敲（和 0.2.x 设置页的行为一致）。
///
/// 返回 `(是否有改动, 新值)`。
fn profile_combo(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    width: f32,
) -> (bool, String) {
    const PRESETS: [&str; 2] = ["web", "desktop"];
    let mut text = current.to_string();
    let mut changed = false;

    if ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(width)
                .hint_text(tr!("profile 名")),
        )
        .changed()
    {
        changed = true;
    }

    let trimmed = text.trim().to_string();
    let is_preset = PRESETS.contains(&trimmed.as_str());
    egui::ComboBox::from_id_salt(id)
        .selected_text(if is_preset { trimmed.as_str() } else { tr!("自定义") })
        .width(96.0)
        .show_ui(ui, |ui| {
            for p in PRESETS {
                if ui.selectable_label(trimmed == p, p).clicked() && trimmed != p {
                    text = p.to_string();
                    changed = true;
                }
            }
            if ui.selectable_label(!is_preset, tr!("自定义")).clicked() && is_preset {
                // 从预设切到自定义：清空输入框，直接开始敲
                text.clear();
                changed = true;
            }
        });

    (changed, text)
}

/// 实例状态对应的小圆点颜色（侧栏用）。
fn phase_dot(phase: &instances::Phase) -> Option<egui::Color32> {
    Some(match phase {
        instances::Phase::Running => theme::ok(),
        instances::Phase::Starting => theme::warn(),
        instances::Phase::Failed(_) => theme::err(),
        instances::Phase::Stopped => theme::dim(),
    })
}

/// 给用户看的版本文案：**总是具体版本号**，不写"全局安装"。
///
/// "全局安装"是"从哪儿来的"，不是"哪个版本"——用户真正想知道的是 0.1.7-rc.2 这种
/// 具体版本（尤其要对照 bug 时）。所以：
///
/// * 实例钉了版本 → 就是那个版本号；
/// * 没钉版本（用全局安装那一份）→ 显示**全局安装的版本号**，后面标个"全局"说明来源；
/// * 全局那份还没探测出来 → 退回到"全局安装"四个字，总比空着强。
fn version_text(cfg_version: &str, global: Option<&str>) -> String {
    let pinned = cfg_version.trim();
    if !pinned.is_empty() {
        return pinned.to_string();
    }
    match global.map(str::trim).filter(|g| !g.is_empty()) {
        Some(g) => trf!("{}（全局）", g),
        None => tr!("全局安装").to_string(),
    }
}

/// 版本列表里的一行：版本号 + 标签 + 日期 + 右侧一个动作按钮。
///
/// 版本列表里的一行：版本号 + 标签 + 日期 + 右侧动作按钮。
///
/// 返回**点了哪个**按钮：`VersionRowAct::None` / `Primary` / `Secondary`。
/// 抽出来是因为这一页分好几组行（已装的、可装的、卸载），排版应当一致——
/// 手画几遍必然长歪。
///
/// * `note`：右侧的动作**之前**显示的状态说明（"该实例在用" / "全局安装在用"）
/// * `primary`：主按钮 `(文字, 悬停说明)`；`None` 表示不给主按钮
/// * `secondary`：次按钮（如「卸载」）。没有主按钮时它就是唯一按钮，也用普通样式，
///   免得"次要"样式让唯一的操作看着像不可点
fn version_row(
    ui: &mut egui::Ui,
    v: &versions::VersionInfo,
    latest: &str,
    note: Option<&str>,
    primary: Option<(String, String)>,
    secondary: Option<(String, String)>,
) -> VersionRowAct {
    let mut act = VersionRowAct::None;
    let has_primary = primary.is_some();
    let row_bg = if note.is_some() { theme::accent_soft() } else { theme::card() };
    egui::Frame::NONE
        .fill(row_bg)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .stroke(egui::Stroke::new(1.0, theme::border()))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut title = v.version.clone();
                if v.version == latest {
                    title.push_str("   ★ latest");
                }
                if !v.tag.is_empty() && v.tag != "latest" {
                    title.push_str(&format!("   [{}]", v.tag));
                }
                ui.label(
                    egui::RichText::new(title).size(14.0).color(if note.is_some() {
                        theme::ok()
                    } else {
                        theme::text()
                    }),
                );
                if !v.published.is_empty() {
                    let d = v.published.split('T').next().unwrap_or("");
                    ui.label(egui::RichText::new(d).size(12.0).color(theme::dim()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((label, hint)) = primary {
                        if ui.button(label).on_hover_text(hint).clicked() {
                            act = VersionRowAct::Primary;
                        }
                    }
                    if let Some((label, hint)) = secondary {
                        let b = if has_primary {
                            ui.button(label)
                        } else {
                            // 唯一的操作不该用小号"次要"样式
                            ui.button(egui::RichText::new(label).size(14.0))
                        };
                        if b.on_hover_text(hint).clicked() {
                            act = VersionRowAct::Secondary;
                        }
                    }
                    if let Some(n) = note {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(n).size(12.5).color(theme::ok()));
                    }
                });
            });
        });
    act
}

/// 版本行上点了哪个按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionRowAct {
    None,
    Primary,
    Secondary,
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn fmt_dur(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        trf!("{} 小时 {} 分", h, m)
    } else if m > 0 {
        trf!("{} 分 {} 秒", m, s)
    } else {
        trf!("{} 秒", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 界面上显示的必须是**具体版本号**，不是"全局安装"四个字——
    /// 用户要的是"我跑的是哪一版"，而不是"这份从哪儿来"。
    #[test]
    fn version_text_always_shows_a_concrete_version() {
        // 钉了版本：原样显示，与全局那份无关
        assert_eq!(version_text("0.1.7-rc.1", Some("0.1.7-rc.2")), "0.1.7-rc.1");
        assert_eq!(version_text("0.1.7-rc.1", None), "0.1.7-rc.1");

        // 没钉版本（用全局安装那一份）：显示全局那份的版本号，并标出来源
        let g = version_text("", Some("0.1.7-rc.2"));
        assert!(g.contains("0.1.7-rc.2"), "要显示具体版本号，实际: {g}");
        assert_ne!(g, "全局安装", "不能只写『全局安装』");

        // 全局那份还没探测出来：退回标签，总比空着强
        let unknown = version_text("", None);
        assert!(!unknown.is_empty());
        // 空白也算"没探测到"。这条**不能**和另一次 tr! 调用比较相等——
        // 语言是进程级状态，并发的 i18n 测试中途切换就会让两次结果对不上。
        // 改成断言"它和『给了个真实版本号』的结果不同"，就与语言无关了。
        assert_ne!(
            version_text("", Some("   ")),
            version_text("", Some("0.1.7-rc.2")),
            "纯空白的版本号应当被当成『没探测到』"
        );

        // 前后空白不该漏进显示
        assert_eq!(version_text("  0.1.6  ", Some("0.1.7")), "0.1.6");
    }
}

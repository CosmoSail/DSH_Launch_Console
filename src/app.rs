//! 启动器主界面。
//!
//! 布局：顶栏（图标 + 名称 + 全局状态）+ 左侧导航（控制台 / 版本 / 插件 / 设置）
//! + 中央内容区。所有耗时操作（npm 检索、插件市场、启动等待）都放后台线程，
//! 结果经 channel 回主线程，UI 永不阻塞。

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::config;
use crate::dsh::{self, DshInstance};
use crate::icon;
use crate::plugins::{self, InstalledPlugin, PluginInfo};
use crate::settings::{CloseAction, Settings, ThemeMode};
use crate::versions::{self, Installed, VersionList};
use crate::webui;
use crate::tray::{self, TrayAction};

// ------------------------------------------------------------------ 阶段

/// DSH 运行阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DshPhase {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

/// 主标签页。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Console,
    Versions,
    Plugins,
    Settings,
}

// ------------------------------------------------------------------ 后台任务

/// 后台任务结果。
enum TaskResult {
    Versions(Result<VersionList, String>),
    Market(Result<Vec<PluginInfo>, String>),
    Github(Result<Vec<PluginInfo>, String>),
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
    tab: Tab,
    settings: Settings,

    /// 后台任务
    tasks: Tasks,

    /// DSH 实例（同步持有，避免跨线程传递非 Send 的 Child）
    instance: Option<DshInstance>,
    phase: DshPhase,
    /// 启动超时时刻
    boot_deadline: Option<std::time::Instant>,
    /// 本次运行的 token
    token: Option<String>,
    /// 运行时长起点
    running_since: Option<std::time::Instant>,

    /// 版本页
    versions: VersionList,
    versions_fetched: bool,
    installed: Installed,
    /// 正在安装的版本（显示进度）
    installing: Option<String>,

    /// 插件页
    market: Vec<PluginInfo>,
    market_loaded: bool,
    market_query: String,
    market_category: String,
    github_results: Vec<PluginInfo>,
    github_loading: bool,
    installed_plugins: Vec<InstalledPlugin>,
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
        // 设置要最先读：风格决定后面整套配色的取值
        let settings = Settings::load();
        theme::set_dark(settings.theme == ThemeMode::Dark);

        let font = icon::install_cjk_font(&cc.egui_ctx);
        apply_theme(&cc.egui_ctx);
        let font_note = match &font {
            Some(p) => format!("中文字体: {}", p),
            None => "未找到系统中文字体，中文可能显示为方块".to_string(),
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
            tab: Tab::Console,
            settings,
            tasks: Tasks::new(),
            instance: None,
            phase: DshPhase::Stopped,
            boot_deadline: None,
            token: None,
            running_since: None,
            versions: VersionList::default(),
            versions_fetched: false,
            installed: Installed::default(),
            installing: None,
            market: Vec::new(),
            market_loaded: false,
            market_query: String::new(),
            market_category: "全部".to_string(),
            github_results: Vec::new(),
            github_loading: false,
            installed_plugins: Vec::new(),
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
        app.refresh_versions(&cc.egui_ctx);
        app.refresh_plugins(&cc.egui_ctx);
        // 若 DSH 已在运行（用户之前开的），直接进入 Running（附加模式）
        let (host, port) = app.settings.host_port();
        if webui::listening(&host, port) {
            app.phase = DshPhase::Running;
            app.running_since = Some(std::time::Instant::now());
            app.token = dsh::latest_token(0);
            config::log("DSH already listening; attaching");
        }
        app
    }

    // ---------------------------------------------------------- 操作

    /// 实际生效的 profile：设置里为空时回落到默认 web
    /// （自定义模式允许输入框暂时为空，不能把空串传给 dsh）。
    fn profile(&self) -> String {
        let p = self.settings.profile.trim();
        if p.is_empty() {
            config::DEFAULT_PROFILE.to_string()
        } else {
            p.to_string()
        }
    }

    fn refresh_versions(&mut self, ctx: &egui::Context) {
        self.installed = versions::installed();
        self.tasks.spawn(ctx, || TaskResult::Versions(versions::fetch_remote()));
    }

    fn refresh_plugins(&mut self, ctx: &egui::Context) {
        let profile = self.profile();
        self.installed_plugins = plugins::installed(&profile);
        if !self.market_loaded {
            self.tasks.spawn(ctx, || TaskResult::Market(plugins::fetch_market()));
        }
    }

    /// 启动 DSH（spawn 很快，同步做；就绪等待交给 poll_dsh 轮询）。
    ///
    /// - **已在运行**（本启动器拉起的，或用户自己开的）→ 绝不重复起服务，
    ///   只在系统浏览器里**新开一个 Web UI 标签页**（这是「再点一次启动」的语义）
    /// - 未运行 → 隐藏终端拉起服务
    fn start_dsh(&mut self) {
        if self.phase == DshPhase::Starting {
            return; // 启动中，忽略重复点击
        }
        let (host, port) = self.settings.host_port();

        // 已在运行 → 只新开一个 Web UI（这是用户的显式动作，不受「自动打开」设置约束）
        if webui::listening(&host, port) {
            if self.phase != DshPhase::Running {
                self.phase = DshPhase::Running;
                self.running_since = Some(std::time::Instant::now());
            }
            if self.token.is_none() {
                let offset = self.instance.as_ref().map(|i| i.log_offset).unwrap_or(0);
                self.token = dsh::latest_token(offset);
            }
            match webui::open_web_ui(&self.settings.url, self.token.as_deref()) {
                Ok(()) => {
                    self.toast = Some(("已在系统浏览器中新开一个 Web UI".into(), false))
                }
                Err(e) => self.toast = Some((e, true)),
            }
            return;
        }

        let entry = match dsh::resolve_entry() {
            Ok(e) => e,
            Err(msg) => {
                self.phase = DshPhase::Failed(msg.clone());
                self.toast = Some((msg, true));
                return;
            }
        };
        config::log(&format!("using DSH entry: {}", entry.source));

        match dsh::spawn(&entry, &host, port, &self.profile()) {
            Ok(inst) => {
                self.token = None;
                self.instance = Some(inst);
                self.phase = DshPhase::Starting;
                self.boot_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(180));
                self.toast = Some(("正在启动 DeepSeek Harness…".into(), false));
            }
            Err(msg) => {
                self.phase = DshPhase::Failed(msg.clone());
                self.toast = Some((msg, true));
            }
        }
    }

    /// 关闭 DSH（整树）。
    fn stop_dsh(&mut self) {
        if let Some(mut inst) = self.instance.take() {
            config::log(&format!("stopping DSH tree pid={}", inst.pid));
            inst.shutdown();
        }
        self.phase = DshPhase::Stopped;
        self.token = None;
        self.running_since = None;
        self.boot_deadline = None;
        self.toast = Some(("已关闭 DeepSeek Harness".into(), false));
    }

    /// 打开 Web UI。
    fn open_web_ui(&mut self) {
        match webui::open_web_ui(&self.settings.url, self.token.as_deref()) {
            Ok(()) => self.toast = Some(("已在系统浏览器中打开 Web UI".into(), false)),
            Err(e) => self.toast = Some((e, true)),
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
                    format!("全局安装已是 {}", v),
                    Ok(format!("{}\n\n已执行: npm i -g {}", text, spec)),
                ),
                Err(e) => TaskResult::Op(format!("切换 {} 失败", v), Err(format!("{}\n{}", text, e))),
            }
        });
    }

    /// 插件：安装。
    fn install_plugin(&mut self, ctx: &egui::Context, p: PluginInfo) {
        let spec = if !p.npm.is_empty() { p.npm.clone() } else { p.install.clone() };
        if spec.is_empty() {
            self.toast = Some(("该插件没有可用的安装标识".into(), true));
            return;
        }
        self.plugin_busy = Some(format!("正在安装 {} …", p.name));
        let profile = self.profile();
        let label = p.name.clone();
        self.tasks.spawn(ctx, move || {
            match plugins::install(&profile, &spec) {
                Ok(text) => TaskResult::Op(format!("已安装 {}", label), Ok(text)),
                Err(e) => TaskResult::Op(format!("安装 {} 失败", label), Err(e)),
            }
        });
    }

    /// 插件：卸载。
    fn uninstall_plugin(&mut self, ctx: &egui::Context, package: String) {
        self.plugin_busy = Some(format!("正在卸载 {} …", package));
        let profile = self.profile();
        let label = package.clone();
        self.tasks.spawn(ctx, move || match plugins::uninstall(&profile, &package) {
            Ok(text) => TaskResult::Op(format!("已卸载 {}", label), Ok(text)),
            Err(e) => TaskResult::Op(format!("卸载 {} 失败", label), Err(e)),
        });
    }

    /// 插件：启用/禁用。
    fn toggle_plugin(&mut self, ctx: &egui::Context, id: String, enabled: bool) {
        let profile = self.profile();
        let label = id.clone();
        let r = plugins::toggle(&profile, &id, enabled);
        match r {
            Ok(()) => {
                self.installed_plugins = plugins::installed(&profile);
                self.toast = Some((
                    format!("{} 已{}", label, if enabled { "启用" } else { "禁用" }),
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
                TrayAction::StartDsh => self.start_dsh(),
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
                        self.toast = Some((format!("版本列表获取失败：{}", e), true));
                    }
                }
                TaskResult::Market(Ok(list)) => {
                    self.market = list;
                    self.market_loaded = true;
                }
                TaskResult::Market(Err(e)) => {
                    self.toast = Some((format!("插件市场加载失败：{}", e), true));
                }
                TaskResult::Github(Ok(list)) => {
                    self.github_results = list;
                    self.github_loading = false;
                }
                TaskResult::Github(Err(e)) => {
                    self.github_loading = false;
                    self.toast = Some((format!("GitHub 搜索失败：{}", e), true));
                }
                TaskResult::Op(title, res) => {
                    self.installing = None;
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
                    self.installed_plugins = plugins::installed(&self.profile());
                }
            }
        }
        if self.tasks.inflight > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// 轮询 DSH 就绪 / 超时 / 提前退出。
    fn poll_dsh(&mut self) {
        let (host, port) = self.settings.host_port();
        if self.phase != DshPhase::Starting {
            // token 迟到时的补抓：只对本启动器自己拉起的实例做（外部实例不知道偏移）
            if self.phase == DshPhase::Running && self.token.is_none() {
                if let Some(inst) = self.instance.as_ref() {
                    self.token = dsh::latest_token(inst.log_offset);
                }
            }
            // 运行中：顺便感知外部把 DSH 关掉了
            if self.phase == DshPhase::Running
                && self.instance.is_some()
                && !webui::listening(&host, port)
            {
                if let Some(inst) = self.instance.as_mut() {
                    if let Some(code) = inst.exited() {
                        config::log(&format!("DSH exited with code {}", code));
                        self.instance = None;
                        self.phase = DshPhase::Stopped;
                        self.running_since = None;
                        self.toast = Some((format!("DSH 已退出（退出码 {}）", code), true));
                    }
                }
            }
            return;
        }

        // 就绪？
        if webui::listening(&host, port) {
            let offset = self.instance.as_ref().map(|i| i.log_offset).unwrap_or(0);
            // DSH 是「先监听端口，后打印带 token 的地址」，两者相差几十毫秒。
            // 抓不到就等一会儿：否则会把不带 token 的地址丢给浏览器，直接吃 401。
            self.token = wait_for_token(offset, std::time::Duration::from_millis(1500));
            self.phase = DshPhase::Running;
            self.running_since = Some(std::time::Instant::now());
            self.boot_deadline = None;
            config::log(&format!(
                "DSH web UI is up (token: {})",
                if self.token.is_some() { "got" } else { "pending" }
            ));
            self.toast = Some(("DeepSeek Harness 已启动".into(), false));
            if self.settings.auto_open_browser {
                let _ = webui::open_web_ui(&self.settings.url, self.token.as_deref());
            }
            return;
        }

        // 提前退出？
        if let Some(inst) = self.instance.as_mut() {
            if let Some(code) = inst.exited() {
                let tail = dsh::log_tail(20);
                config::log(&format!("DSH exited early with code {}", code));
                self.instance = None;
                let msg = format!(
                    "DSH 启动失败（退出码 {}）。\n最近输出：\n{}",
                    code,
                    if tail.is_empty() { "（无）".to_string() } else { tail }
                );
                self.phase = DshPhase::Failed(msg.clone());
                self.toast = Some(("DSH 启动失败".into(), true));
                self.last_output = Some(msg);
                return;
            }
        }

        // 超时？
        if let Some(dl) = self.boot_deadline {
            if std::time::Instant::now() >= dl {
                self.stop_dsh();
                self.phase = DshPhase::Failed("DSH 在 180 秒内未就绪".into());
                self.toast = Some(("DSH 启动超时".into(), true));
            }
        }
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
        if self.phase == DshPhase::Starting
            || (self.phase == DshPhase::Running
                && self.token.is_none()
                && self.instance.is_some())
        {
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

        if self.toast.is_some() {
            egui::Panel::bottom("toast")
                .frame(
                    egui::Frame::NONE
                        .fill(if self.toast.as_ref().map(|t| t.1).unwrap_or(false) {
                            theme::err_soft()
                        } else {
                            theme::ok_soft()
                        })
                        .inner_margin(egui::Margin::symmetric(16, 12)),
                )
                .show(ui, |ui| self.toast_bar(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::bg()).inner_margin(egui::Margin::same(16)))
            .show(ui, |ui| match self.tab {
                Tab::Console => self.tab_console(ui),
                Tab::Versions => self.tab_versions(ui),
                Tab::Plugins => self.tab_plugins(ui),
                Tab::Settings => self.tab_settings(ui),
            });

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
                            egui::RichText::new("DeepSeek Harness 启动器")
                                .size(12.5)
                                .color(theme::dim()),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (fg, bg, label) = match &self.phase {
                            DshPhase::Running => (theme::ok(), theme::ok_soft(), "运行中"),
                            DshPhase::Starting => (theme::warn(), theme::warn_soft(), "启动中…"),
                            DshPhase::Stopped => (theme::dim(), theme::surface(), "未启动"),
                            DshPhase::Failed(_) => (theme::err(), theme::err_soft(), "启动失败"),
                        };
                        status_chip(ui, label, fg, bg);
                    });
                });
            });

        // 顶栏底部分隔线（比整圈描边干净）
        let r = panel.response.rect;
        ui.painter()
            .hline(r.x_range(), r.bottom() - 0.5, egui::Stroke::new(1.0, theme::border()));
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let items = [
            (Tab::Console, "控制台", "启动 / 关闭"),
            (Tab::Versions, "版本", "安装与切换"),
            (Tab::Plugins, "插件", "搜索与管理"),
            (Tab::Settings, "设置", "服务与行为"),
        ];
        for (tab, name, hint) in items {
            let selected = self.tab == tab;
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 52.0),
                egui::Sense::click(),
            );
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
                    egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 12.0), egui::vec2(3.0, 28.0));
                ui.painter().rect_filled(bar, 2.0, theme::accent());
            }
            // 选中项用主色，未选中用正文色（浅底上要够黑才清楚）
            let text_col = if selected { theme::accent() } else { theme::text() };
            ui.painter().text(
                rect.min + egui::vec2(16.0, 14.0),
                egui::Align2::LEFT_TOP,
                name,
                egui::FontId::proportional(15.0),
                text_col,
            );
            ui.painter().text(
                rect.min + egui::vec2(16.0, 34.0),
                egui::Align2::LEFT_TOP,
                hint,
                egui::FontId::proportional(11.5),
                theme::dim(),
            );
            if resp.clicked() {
                self.tab = tab;
            }
            ui.add_space(4.0);
        }

        // 底部：版本信息
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                    .size(11.5)
                    .color(theme::dim()),
            );
        });
    }

    // ------------------------------------------------------ 控制台

    fn tab_console(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("控制台").size(21.0).color(theme::text()));
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new("启动或关闭 DeepSeek Harness，界面在系统默认浏览器中打开")
                .size(13.0)
                .color(theme::dim()),
        );
        ui.add_space(14.0);

        // —— DSH 控制卡 ——
        card(ui, |ui| {
            ui.horizontal(|ui| {
                let (dot, text) = match &self.phase {
                    DshPhase::Running => (theme::ok(), "DeepSeek Harness 正在运行".to_string()),
                    DshPhase::Starting => (theme::warn(), "正在启动 DeepSeek Harness…".to_string()),
                    DshPhase::Stopped => (theme::dim(), "DeepSeek Harness 未启动".to_string()),
                    DshPhase::Failed(_) => (theme::err(), "启动失败".to_string()),
                };
                let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 6.0, dot);
                ui.add_space(6.0);
                ui.label(egui::RichText::new(text).size(15.5).strong().color(theme::text()));

                if let Some(since) = self.running_since {
                    let s = since.elapsed().as_secs();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "已运行 {}",
                                fmt_dur(s)
                            ))
                            .size(13.0)
                            .color(theme::dim()),
                        );
                    });
                }
            });

            let (host, port) = self.settings.host_port();
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!(
                    "地址  {}      端口 {}      profile {}",
                    host,
                    port,
                    self.profile()
                ))
                .size(12.5)
                .color(theme::dim()),
            );

            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let running = matches!(self.phase, DshPhase::Running | DshPhase::Starting);
                let starting = self.phase == DshPhase::Starting;
                // 只有「本启动器拉起的」实例才谈得上关闭（外部启动的绝不误杀）
                let stoppable = running && self.instance.is_some();

                // 启动 / 关闭 —— 专用按键
                // 运行中仍然可点：点了 = 在浏览器新开一个 Web UI，不会重复起服务
                let start_btn = egui::Button::new(
                    egui::RichText::new("▶  启动 DeepSeek Harness")
                        .size(15.0)
                        .strong()
                        .color(if starting { theme::dim() } else { egui::Color32::WHITE }),
                )
                .fill(if starting {
                    theme::surface()
                } else if running {
                    theme::accent_solid()
                } else {
                    theme::ok_solid()
                })
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(224.0, 42.0))
                .corner_radius(10.0);
                if ui.add_enabled(!starting, start_btn).clicked() {
                    self.start_dsh();
                }

                ui.add_space(8.0);

                let stop_btn = egui::Button::new(
                    egui::RichText::new("■  关闭 DeepSeek Harness")
                        .size(15.0)
                        .strong()
                        .color(if stoppable { egui::Color32::WHITE } else { theme::dim() }),
                )
                .fill(if stoppable { theme::err_solid() } else { theme::surface() })
                .stroke(egui::Stroke::NONE)
                .min_size(egui::vec2(224.0, 42.0))
                .corner_radius(10.0);
                if ui.add_enabled(stoppable, stop_btn).clicked() {
                    self.stop_dsh();
                }
            });

            ui.add_space(10.0);

            // 打开 Web UI
            let live = matches!(self.phase, DshPhase::Running);
            let open_btn = egui::Button::new(
                egui::RichText::new("🌐  在浏览器中打开 Web UI")
                    .size(15.0)
                    .strong()
                    .color(if live { egui::Color32::WHITE } else { theme::dim() }),
            )
            .fill(if live { theme::accent_solid() } else { theme::surface() })
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(456.0, 42.0))
            .corner_radius(10.0);
            if ui.add_enabled(live, open_btn).clicked() {
                self.open_web_ui();
            }
            if matches!(self.phase, DshPhase::Running) && self.instance.is_none() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "注意：当前 DSH 不是本启动器启动的，为避免误杀，启动器不会结束它（也不会随启动器退出而被关闭）。",
                    )
                    .size(12.0)
                    .color(theme::warn()),
                );
            }
        });

        ui.add_space(12.0);

        // —— 失败 / 日志 ——
        if let DshPhase::Failed(msg) = self.phase.clone() {
            card_err(ui, |ui| {
                ui.label(egui::RichText::new("启动失败").size(15.0).strong().color(theme::err()));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&msg).size(12.5).color(theme::text()));
            });
            ui.add_space(12.0);
        }

        card(ui, |ui| {
            egui::CollapsingHeader::new(
                egui::RichText::new("运行日志").size(15.0).color(theme::text()),
            )
            .default_open(false)
            .show(ui, |ui| {
                // 日志区**可滚动**：滚轮或右侧滚动条都能翻历史行。
                // 文本框按内容行数撑高，滚动交给 ScrollArea（否则会和文本框自身
                // 的内部滚动打架，滚轮就不好使了）。
                let tail = dsh::log_tail(400);
                log_view(ui, &tail, 280.0);

                if let Some(out) = &self.last_output {
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("最近操作输出").size(13.5).color(theme::dim()));
                    log_view(ui, out, 180.0);
                }
            });
        });
    }

    // ------------------------------------------------------ 版本

    fn tab_versions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("版本管理").size(21.0).color(theme::text()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("刷新").clicked() {
                    self.refresh_versions(ui.ctx());
                }
                ui.add_space(6.0);
                if ui.button("源码仓库").clicked() {
                    if let Err(e) = webui::open_url(config::DSH_REPO) {
                        self.toast = Some((e, true));
                    }
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(
                "自动从 npm 检索 @deepseek-ai/dsh 的全部版本（与 GitHub 源码仓库同一来源）。在这里安装 = 直接切换全局安装：npm i -g @deepseek-ai/dsh@<版本>。",
            )
            .size(13.0)
            .color(theme::dim()),
        );
        ui.add_space(10.0);

        // 全局安装（DSH 只有这一份）
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("全局安装").size(14.0).color(theme::dim()));
                ui.add_space(8.0);
                let cur = match &self.installed.global {
                    Some(g) => g.clone(),
                    None => "（未检测到）".to_string(),
                };
                let col = if self.installed.global.is_some() { theme::ok() } else { theme::warn() };
                ui.label(egui::RichText::new(cur).size(14.0).strong().color(col));
            });
            if self.installed.global.is_none() {
                ui.add_space(4.0);
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "还没检测到全局安装。可以在下面选一个版本点「安装」，或手动执行 npm i -g @deepseek-ai/dsh。",
                    )
                    .size(12.0)
                    .color(theme::warn()),
                );
            }
            if self.instance.is_some() {
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "注意：DSH 正在运行（由本启动器启动）。切换版本会覆盖全局安装里的文件，Windows 上可能因文件占用失败——建议先在「控制台」关闭它。",
                    )
                    .size(12.0)
                    .color(theme::warn()),
                );
            }
        });

        ui.add_space(12.0);

        // 可选版本
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("可选版本").size(14.0).strong().color(theme::text()));
            if let Some(t) = &self.installing {
                ui.add_space(8.0);
                ui.spinner();
                ui.label(
                    egui::RichText::new(format!("正在把全局安装切换为 {} …", t))
                        .size(12.5)
                        .color(theme::warn()),
                );
            }
        });
        ui.add_space(6.0);

        if self.versions.versions.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new("正在获取版本列表…").size(13.0).color(theme::dim()));
            });
            return;
        }

        let current = self.installed.global.clone();
        let has_any = current.is_some();
        let latest = self.versions.latest.clone();
        let list = self.versions.versions.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for v in list.iter() {
                let active = current.as_deref() == Some(v.version.as_str());
                let row_bg = if active { theme::accent_soft() } else { theme::card() };
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
                                egui::RichText::new(title)
                                    .size(14.0)
                                    .color(if active { theme::ok() } else { theme::text() }),
                            );
                            if !v.published.is_empty() {
                                let d = v.published.split('T').next().unwrap_or("");
                                ui.label(egui::RichText::new(d).size(12.0).color(theme::dim()));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if active {
                                        ui.label(
                                            egui::RichText::new("使用中").size(12.5).color(theme::ok()),
                                        );
                                    } else {
                                        // 已有全局安装时是「切换」，没有时是「安装」——动作一样
                                        let label = if has_any { "切换" } else { "安装" };
                                        if ui
                                            .add_enabled(
                                                self.installing.is_none(),
                                                egui::Button::new(label),
                                            )
                                            .clicked()
                                        {
                                            self.install_version(ui.ctx(), v.version.clone());
                                        }
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(4.0);
            }
        });
    }

    // ------------------------------------------------------ 插件

    fn tab_plugins(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("插件管理").size(21.0).color(theme::text()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.market_loaded {
                    ui.label(
                        egui::RichText::new(format!("市场共 {} 个插件", self.market.len()))
                            .size(12.5)
                            .color(theme::dim()),
                    );
                }
            });
        });
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(format!(
                "搜索并管理 DeepSeek Harness 插件（profile: {}）。启停写入 cordis.patch.yml。",
                self.profile()
            ))
            .size(13.0)
            .color(theme::dim()),
        );
        ui.add_space(10.0);

        // 搜索行
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.market_query)
                    .hint_text("搜索插件名称 / 作者 / 描述…")
                    .desired_width(300.0),
            );
            if ui.button("搜索 GitHub").clicked() {
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
                    "全部".to_string()
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

        // —— 已安装 ——
        if !self.installed_plugins.is_empty() {
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("已安装（{}）", self.installed_plugins.len()))
                    .size(14.5)
                    .strong()
                    .color(theme::text()),
            )
            .default_open(true)
            .show(ui, |ui| {
                let list = self.installed_plugins.clone();
                for p in list.iter() {
                    egui::Frame::NONE
                        .fill(theme::card())
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(12, 8))
                        .stroke(egui::Stroke::new(1.0, theme::border()))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(&p.package)
                                        .size(14.0)
                                        .strong()
                                        .color(if p.enabled { theme::text() } else { theme::dim() }),
                                );
                                if !p.version.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&p.version).size(12.0).color(theme::dim()),
                                    );
                                }
                                if p.bundled {
                                    ui.label(
                                        egui::RichText::new("bundle").size(11.5).color(theme::accent()),
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("卸载").clicked() {
                                            self.uninstall_plugin(ui.ctx(), p.package.clone());
                                        }
                                        if let Some(id) = &p.id {
                                            let label = if p.enabled { "禁用" } else { "启用" };
                                            if ui.button(label).clicked() {
                                                self.toggle_plugin(
                                                    ui.ctx(),
                                                    id.clone(),
                                                    !p.enabled,
                                                );
                                            }
                                        }
                                    },
                                );
                            });
                            if let Some(id) = &p.id {
                                ui.label(
                                    egui::RichText::new(format!("id: {}", id))
                                        .size(12.5)
                                        .color(theme::dim()),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("未找到 cordis 插件 id，无法启停")
                                        .size(12.5)
                                        .color(theme::warn()),
                                );
                            }
                        });
                    ui.add_space(4.0);
                }
            });
            ui.add_space(10.0);
        }

        // —— GitHub 搜索结果 ——
        if !self.github_results.is_empty() {
            ui.label(
                egui::RichText::new(format!("GitHub 搜索结果（{}）", self.github_results.len()))
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
                    egui::RichText::new("正在加载插件市场…").size(14.0).color(theme::dim()),
                );
                if ui.button("重新加载").clicked() {
                    self.tasks.spawn(ui.ctx(), || TaskResult::Market(plugins::fetch_market()));
                }
            });
            return;
        }

        let filtered =
            plugins::filter_local(&self.market, &self.market_query, &self.market_category);
        ui.label(
            egui::RichText::new(format!("插件市场（{} 个结果）", filtered.len()))
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

    // ------------------------------------------------------ 设置

    fn tab_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading(egui::RichText::new("设置").size(21.0).color(theme::text()));
        ui.add_space(12.0);

        let mut changed = false;

        // 风格：浅色 / 深色（切换立即生效，并写进设置）
        card(ui, |ui| {
            ui.label(egui::RichText::new("风格").size(14.0).strong().color(theme::text()));
            ui.add_space(8.0);
            let mut dark = self.settings.theme == ThemeMode::Dark;
            let before = dark;
            ui.radio_value(&mut dark, false, "浅色");
            ui.radio_value(&mut dark, true, "深色");
            if dark != before {
                self.settings.theme = if dark { ThemeMode::Dark } else { ThemeMode::Light };
                theme::set_dark(dark);
                apply_theme(ui.ctx());
                ui.ctx().request_repaint();
                changed = true;
            }
        });

        ui.add_space(10.0);

        card(ui, |ui| {
            ui.label(egui::RichText::new("服务地址").size(14.0).strong().color(theme::text()));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.url)
                        .desired_width(300.0),
                );
                if ui.button("应用").clicked() {
                    changed = true;
                }
            });
            ui.label(
                egui::RichText::new("DSH Web UI 的地址，host/port 同时用于启动参数与端口探测。")
                    .size(12.0)
                    .color(theme::dim()),
            );
        });

        ui.add_space(10.0);

        card(ui, |ui| {
            ui.label(egui::RichText::new("Profile").size(14.0).strong().color(theme::text()));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                const PRESETS: [&str; 2] = ["web", "desktop"];
                // 「当前是哪个模式」完全由输入框里的内容决定：
                // 输入 web / desktop 就是对应预设，其它任何值就是自定义。
                // 所以不需要额外的状态字段，手输与下拉永远一致。
                let text = self.settings.profile.trim().to_string();
                let is_preset = PRESETS.contains(&text.as_str());

                // 输入框：直接手输
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.settings.profile)
                        .desired_width(150.0)
                        .hint_text("profile 名"),
                );
                if resp.changed() {
                    changed = true;
                }

                // 选项框：显示当前落在哪个预设上；选预设即填入，选自定义即清空等你输
                let mode = if is_preset { text.as_str() } else { "自定义" };
                egui::ComboBox::from_id_salt("profile-preset")
                    .selected_text(mode)
                    .width(96.0)
                    .show_ui(ui, |ui| {
                        for p in PRESETS {
                            if ui.selectable_label(text == p, p).clicked() && text != p {
                                self.settings.profile = p.to_string();
                                changed = true;
                            }
                        }
                        if ui.selectable_label(!is_preset, "自定义").clicked() && is_preset {
                            // 从预设切到自定义：清空输入框，直接开始敲
                            self.settings.profile.clear();
                            changed = true;
                        }
                    });
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("决定启动哪个 profile；插件也装进这一份。")
                    .size(12.0)
                    .color(theme::dim()),
            );
            if self.settings.profile.trim().is_empty() {
                ui.label(
                    egui::RichText::new("profile 不能为空，将按 web 处理。")
                        .size(12.0)
                        .color(theme::warn()),
                );
            }
        });

        ui.add_space(10.0);

        // 关闭按钮行为（✕ 是收进托盘还是直接退出）
        card(ui, |ui| {
            ui.label(egui::RichText::new("关闭按钮行为").size(14.0).strong().color(theme::text()));
            ui.add_space(8.0);
            let mut to_tray = self.settings.close_action == CloseAction::Tray;
            let before = to_tray;
            ui.radio_value(&mut to_tray, true, "最小化到系统托盘（后台继续运行）");
            ui.radio_value(&mut to_tray, false, "直接关闭（点 × 即刻退出）");
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
                    egui::RichText::new("当前平台不支持系统托盘，关闭按钮将直接退出。")
                        .size(12.0)
                        .color(theme::warn()),
                );
            }
        });

        ui.add_space(10.0);

        card(ui, |ui| {
            let mut auto = self.settings.auto_open_browser;
            if ui
                .checkbox(&mut auto, "启动 DSH 后自动在浏览器打开 Web UI")
                .changed()
            {
                self.settings.auto_open_browser = auto;
                changed = true;
            }
        });

        if changed {
            self.settings.save();
            self.installed_plugins = plugins::installed(&self.profile());
            self.toast = Some(("设置已保存".into(), false));
        }

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

    /// 真正退出：关 DSH、摘托盘。
    fn shutdown(&mut self) {
        if let Some(mut inst) = self.instance.take() {
            config::log(&format!("shutting down DSH tree pid={}", inst.pid));
            inst.shutdown();
        }
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

/// 短轮询等服务日志里的 token；超时返回 None（不阻塞界面超过 `max`）。
fn wait_for_token(offset: u64, max: std::time::Duration) -> Option<String> {
    let deadline = std::time::Instant::now() + max;
    loop {
        if let Some(t) = dsh::latest_token(offset) {
            return Some(t);
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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

fn card_err(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(theme::err_soft())
        .corner_radius(12.0)
        .inner_margin(egui::Margin::same(16))
        .stroke(egui::Stroke::new(1.0, theme::err().gamma_multiply(0.35)))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
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
                    if ui.button("安装").clicked() {
                        action = RowAction::Install(p.clone());
                    }
                    if !p.url.is_empty() && ui.button("仓库").clicked() {
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
        format!("{} 小时 {} 分", h, m)
    } else if m > 0 {
        format!("{} 分 {} 秒", m, s)
    } else {
        format!("{} 秒", s)
    }
}

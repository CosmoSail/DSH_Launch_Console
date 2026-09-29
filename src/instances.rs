//! 多实例：同时跑多个互不干扰的 DSH。
//!
//! 0.2.5 及以前整个程序只有一个实例：一个 `Option<DshInstance>`、一个固定的
//! 服务日志、一个全局安装入口。0.3.1 起一个实例 = 一份 [`InstanceConfig`]
//! （端口 / profile / DSH_HOME / 版本）+ 本进程观察到的运行时状态。
//!
//! 这个模块只放**与界面无关**的实例逻辑：
//!
//! - [`Instance`]：一个实例的运行时状态（进程、阶段、token、运行时长）
//! - [`poll`]：轮询所有实例的就绪 / 超时 / 退出
//!
//! 真正**拉起进程**的动作留在 `app`：它要在启动前后改一串只属于界面的状态
//! （toast、编辑态、选中项），而且 `App` 才是这些 `Instance` 的所有者。
//! 分成两处各自维护一套启动路径是最容易长出 bug 的写法，所以这里刻意不提供
//! `start()`——启动只有一条路，在 `app::App::start_instance`。
//! 同理，"认领已经跑着的实例"也只有一条路（`app::App::attach_existing`）。
//!
//! 几个必须守住的点：
//!
//! - **每个实例一份日志**。`latest_token` 的语义是"从某个偏移之后找 token"，
//!   几个实例写同一个文件就会互相抓到对方的 token（拿别人的 token 去开自己的
//!   界面，结果是 401）。
//! - **每个实例一份 DSH_HOME**（默认开）。共用一份的话，插件管理命令会同时读写
//!   同一个 `profiles/<name>`，两个实例打架。
//! - **只结束自己拉起的那棵树**。外部自己起的 DSH 依然不碰（[`Instance::stoppable`]），
//!   这是从 0.1.0 就有的护栏，多实例下更要注意别杀错。

use std::path::{Path, PathBuf};

use crate::config;
use crate::dsh::DshInstance;
use crate::settings::InstanceConfig;
use crate::{tr, trf};

/// 启动超时：DSH 要挂载整棵 cordis 插件树，机器忙时明显变慢
/// （`tests/test-startup-timing.ps1` 实测波动 2.3s ~ 28.4s）。
pub const START_TIMEOUT_SECS: u64 = 180;

/// 抓 token 的最长等待：DSH 先监听端口、后打印带 token 的地址，相差几十毫秒。
const TOKEN_WAIT_MS: u64 = 1500;

/// 一个实例的运行阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Stopped,
    Starting,
    Running,
    Failed(String),
}

/// 一个实例：配置 + 本进程看到的状态。
pub struct Instance {
    pub cfg: InstanceConfig,
    /// 本启动器拉起的进程；外部已经跑着的实例这里是 `None`
    pub proc: Option<DshInstance>,
    pub phase: Phase,
    /// 启动超时时刻
    pub boot_deadline: Option<std::time::Instant>,
    /// 本次启动解析到的 token
    pub token: Option<String>,
    /// 运行时长起点
    pub running_since: Option<std::time::Instant>,
    /// 正在后台把这个版本装到自管目录（显示进度用）
    pub installing: Option<String>,
    /// 这次安装完成后，要不要顺便把这个实例切到该版本
    /// （在实例控制台里点「装到自管目录」= 我要它跑这个版本，装完就该切过去）
    pub use_after_install: bool,
}

impl Instance {
    pub fn new(cfg: InstanceConfig) -> Self {
        Self {
            cfg,
            proc: None,
            phase: Phase::Stopped,
            boot_deadline: None,
            token: None,
            running_since: None,
            installing: None,
            use_after_install: false,
        }
    }

    /// 本启动器能不能结束它。外部自己起的 DSH 绝不误杀。
    pub fn stoppable(&self) -> bool {
        self.proc.is_some()
    }

    pub fn log_path(&self) -> PathBuf {
        self.cfg.log_path()
    }

    pub fn home(&self) -> PathBuf {
        self.cfg.resolved_home()
    }

    pub fn url(&self, host: &str) -> String {
        self.cfg.url(host)
    }

    /// 配置里指定的版本：空串表示"用全局安装那一份"。
    ///
    /// 注意这不是**给人看**的文案——界面要显示具体版本号，得配上"全局那份是哪个版本"，
    /// 而那个信息在 `App` 手里（见 `app::version_text`）。
    pub fn configured_version(&self) -> &str {
        self.cfg.version.trim()
    }

    /// 阶段对应的一句话。
    pub fn status_text(&self) -> String {
        match &self.phase {
            Phase::Running => {
                if self.proc.is_some() {
                    tr!("运行中").to_string()
                } else {
                    tr!("运行中（外部启动）").to_string()
                }
            }
            Phase::Starting => tr!("启动中…").to_string(),
            Phase::Stopped => tr!("未启动").to_string(),
            Phase::Failed(_) => tr!("启动失败").to_string(),
        }
    }
}

/// 轮询所有实例：就绪 / 超时 / 提前退出 / 被外部关掉。
///
/// 返回 `(发生了状态迁移的提示文案, 仍在启动的实例数)`。
pub fn poll(items: &mut [Instance], host: &str) -> (Vec<String>, usize) {
    let mut events = Vec::new();
    let mut starting = 0usize;
    for inst in items.iter_mut() {
        // token 迟到时补抓（只对自己拉起的实例做——外部实例不知道日志偏移）
        if inst.phase == Phase::Running && inst.token.is_none() {
            if let Some(p) = inst.proc.as_ref() {
                inst.token = crate::dsh::latest_token_at(&inst.log_path(), p.log_offset);
            }
        }

        if inst.phase == Phase::Starting {
            starting += 1;

            // 就绪？
            if webui_listening(host, inst.cfg.port) {
                let off = inst.proc.as_ref().map(|p| p.log_offset).unwrap_or(0);
                // DSH 是「先监听端口，后打印带 token 的地址」，两者相差几十毫秒。
                // 抓不到就等一会儿：否则会把不带 token 的地址丢给浏览器，直接吃 401。
                inst.token = wait_for_token(
                    &inst.log_path(),
                    off,
                    std::time::Duration::from_millis(TOKEN_WAIT_MS),
                );
                inst.phase = Phase::Running;
                inst.running_since = Some(std::time::Instant::now());
                inst.boot_deadline = None;
                events.push(trf!("{} 已启动", inst.cfg.name));
                continue;
            }

            // 提前退出？
            if let Some(p) = inst.proc.as_mut() {
                if let Some(code) = p.exited() {
                    let tail = crate::dsh::log_tail_at(&inst.log_path(), 20);
                    config::log(&format!(
                        "instance {} exited early with code {}",
                        inst.cfg.name, code
                    ));
                    inst.proc = None;
                    inst.boot_deadline = None;
                    inst.phase = Phase::Failed(trf!(
                        "DSH 启动失败（退出码 {}）。\n最近输出：\n{}",
                        code,
                        if tail.is_empty() { tr!("（无）").to_string() } else { tail }
                    ));
                    events.push(trf!("{} 启动失败（退出码 {}）", inst.cfg.name, code));
                    continue;
                }
            }

            // 超时？
            if let Some(dl) = inst.boot_deadline {
                if std::time::Instant::now() >= dl {
                    if let Some(mut p) = inst.proc.take() {
                        p.shutdown();
                    }
                    inst.boot_deadline = None;
                    inst.phase =
                        Phase::Failed(trf!("DSH 在 {} 秒内未就绪", START_TIMEOUT_SECS));
                    events.push(trf!("{} 启动超时", inst.cfg.name));
                }
            }
            continue;
        }

        // 运行中：感知被关掉。只对自己拉起的实例判定——外部实例的端口没在听
        // 并不能说明什么（可能只是还没起来，或者本来就该由别人管）。
        if inst.phase == Phase::Running && inst.proc.is_some() {
            if let Some(p) = inst.proc.as_mut() {
                if let Some(code) = p.exited() {
                    if !webui_listening(host, inst.cfg.port) {
                        config::log(&format!(
                            "instance {} exited with code {}",
                            inst.cfg.name, code
                        ));
                        inst.proc = None;
                        inst.phase = Phase::Stopped;
                        inst.running_since = None;
                        events.push(trf!("{} 已退出（退出码 {}）", inst.cfg.name, code));
                    }
                }
            }
        }
    }
    (events, starting)
}

/// 端口探测。抽成小函数只是为了让上面的逻辑读起来短一点。
fn webui_listening(host: &str, port: u16) -> bool {
    crate::webui::listening(host, port)
}

/// 轮询等服务日志里出现本次运行的 token。
fn wait_for_token(log: &Path, offset: u64, max: std::time::Duration) -> Option<String> {
    let t0 = std::time::Instant::now();
    loop {
        if let Some(t) = crate::dsh::latest_token_at(log, offset) {
            return Some(t);
        }
        if t0.elapsed() >= max {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, port: u16) -> InstanceConfig {
        InstanceConfig {
            id: id.to_string(),
            name: id.to_string(),
            version: String::new(),
            port,
            profile: "web".to_string(),
            own_home: true,
            home: String::new(),
        }
    }

    #[test]
    fn configured_version_is_empty_for_global_install() {
        // 空版本 = 用全局安装那一份；界面把它渲染成什么文案由 app::version_text 决定，
        // 这里只管配置语义。
        let mut c = cfg("a", 3081);
        assert_eq!(Instance::new(c.clone()).configured_version(), "");
        assert!(Instance::new(c.clone()).cfg.version.is_empty());

        c.version = "0.1.6".into();
        assert_eq!(Instance::new(c).configured_version(), "0.1.6");
    }

    #[test]
    fn external_instance_is_not_stoppable() {
        // 外部启动的实例（没有 proc）绝不能被当成"可以关闭"——这是从 0.1.0
        // 就有的护栏，多实例下更容易杀错。
        let mut i = Instance::new(cfg("external", 3081));
        i.phase = Phase::Running;
        assert!(!i.stoppable(), "没有本启动器拉起的进程 → 不可关闭");

        // 自己拉起的进程才算可结束（这里只验证判定依据是 proc 而不是 phase：
        // 没有 proc 时无论什么阶段都不可关闭）
        i.phase = Phase::Starting;
        assert!(!i.stoppable());
        i.phase = Phase::Failed("boom".into());
        assert!(!i.stoppable());
    }

    #[test]
    fn poll_is_safe_with_no_instances() {
        let (events, starting) = poll(&mut [], "127.0.0.1");
        assert!(events.is_empty());
        assert_eq!(starting, 0);
    }

    #[test]
    fn poll_leaves_a_stopped_instance_alone() {
        // 没启动过的实例不该被 poll 改动状态（尤其不能被误判成"退出了"）
        let mut items = vec![Instance::new(cfg("idle", 3081))];
        let (events, starting) = poll(&mut items, "127.0.0.1");
        assert_eq!(items[0].phase, Phase::Stopped);
        assert!(events.is_empty());
        assert_eq!(starting, 0);
    }

    #[test]
    fn poll_skips_dead_ports_entirely() {
        // 3199 上没东西在听，且实例没启动过 → 不该被认领、也不该有事件
        let mut items = vec![Instance::new(cfg("idle", 3199))];
        let (events, starting) = poll(&mut items, "127.0.0.1");
        assert_eq!(items[0].phase, Phase::Stopped);
        assert!(events.is_empty());
        assert_eq!(starting, 0);
    }
}

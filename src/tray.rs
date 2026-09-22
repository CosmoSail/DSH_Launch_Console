#![allow(dead_code)] // 平台相关/预留 API
//! 系统托盘。
//!
//! Windows：经典 Win32 方案——一个 message-only 窗口接收托盘回调，
//! 右键弹出菜单（显示/隐藏窗口、启动/关闭 DSH、打开 Web UI、关闭按钮行为子菜单、退出）。
//! 菜单动作写进共享槽位并 `request_repaint()` 唤醒 egui，由 UI 线程消费。
//!
//! 「关闭按钮行为」用**子菜单 + 单选勾**呈现：当前用的是哪一种，菜单里直接看得见。
//!
//! 其他平台：空实现（`available()` 返回 false），界面自动隐藏托盘相关选项。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// 托盘菜单触发的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// 显示/隐藏主窗口
    ToggleWindow,
    /// 启动 DSH
    StartDsh,
    /// 关闭 DSH
    StopDsh,
    /// 在浏览器打开 Web UI
    OpenWebUi,
    /// 直接指定 ✕ 行为（true = 最小化到托盘）
    SetCloseToTray(bool),
    /// 退出启动器（并关闭 DSH）
    Quit,
}

static PENDING: Mutex<Option<TrayAction>> = Mutex::new(None);

/// ✕ 按钮当前行为（true = 最小化到托盘）。
/// 存一份在这里，是为了让托盘菜单能给「当前生效的那一项」打勾——
/// 否则用户在托盘里根本看不出切换到位没有。
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);

/// 同步当前 ✕ 行为（设置变化时由界面调用）。
pub fn set_close_to_tray(v: bool) {
    CLOSE_TO_TRAY.store(v, Ordering::Relaxed);
}

/// 当前 ✕ 行为（true = 最小化到托盘）。
pub fn close_to_tray() -> bool {
    CLOSE_TO_TRAY.load(Ordering::Relaxed)
}

/// 取出待处理的托盘动作。
pub fn take_action() -> Option<TrayAction> {
    PENDING.lock().ok().and_then(|mut g| g.take())
}

#[cfg(windows)]
mod imp {
    use super::{TrayAction, PENDING};
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = 0x8000 + 1; // WM_APP + 1
    const CMD_TOGGLE: usize = 1;
    const CMD_START: usize = 2;
    const CMD_STOP: usize = 3;
    const CMD_OPEN: usize = 4;
    const CMD_CLOSE_TRAY: usize = 5;
    const CMD_CLOSE_EXIT: usize = 7;
    const CMD_QUIT: usize = 6;

    static ICON: OnceLock<isize> = OnceLock::new();
    static HWND_SLOT: OnceLock<isize> = OnceLock::new();
    static CTX: OnceLock<egui::Context> = OnceLock::new();

    /// 托盘是否可用。
    pub fn available() -> bool {
        HWND_SLOT.get().is_some()
    }

    /// 主窗口句柄（用于显示/隐藏）。
    static MAIN_HWND: OnceLock<isize> = OnceLock::new();
    pub fn set_main_hwnd(h: isize) {
        let _ = MAIN_HWND.set(h);
    }
    pub fn main_hwnd() -> Option<isize> {
        MAIN_HWND.get().copied()
    }

    /// 32×32 RGBA → HICON（DG 位图行序自上而下，不能翻转）。
    fn make_icon() -> isize {
        unsafe {
            let s = crate::icon::LOGO_SIZE as usize;
            let rgba = crate::icon::LOGO_RGBA;
            let mut bgra = vec![0u8; s * s * 4];
            for (i, px) in rgba.chunks_exact(4).enumerate() {
                let d = i * 4;
                bgra[d] = px[2];
                bgra[d + 1] = px[1];
                bgra[d + 2] = px[0];
                bgra[d + 3] = px[3];
            }
            let mask = vec![0u8; s * (s / 8)];
            CreateIcon(
                std::ptr::null_mut(),
                crate::icon::LOGO_SIZE as i32,
                crate::icon::LOGO_SIZE as i32,
                1,
                32,
                mask.as_ptr(),
                bgra.as_ptr(),
            ) as isize
        }
    }

    fn push(action: TrayAction) {
        if let Ok(mut g) = PENDING.lock() {
            *g = Some(action);
        }
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_TRAY {
            match lp as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => push(TrayAction::ToggleWindow),
                WM_RBUTTONUP => {
                    let cmd = popup_menu(hwnd as isize);
                    match cmd {
                        CMD_TOGGLE => push(TrayAction::ToggleWindow),
                        CMD_START => push(TrayAction::StartDsh),
                        CMD_STOP => push(TrayAction::StopDsh),
                        CMD_OPEN => push(TrayAction::OpenWebUi),
                        CMD_CLOSE_TRAY => push(TrayAction::SetCloseToTray(true)),
                        CMD_CLOSE_EXIT => push(TrayAction::SetCloseToTray(false)),
                        CMD_QUIT => push(TrayAction::Quit),
                        _ => {}
                    }
                }
                _ => {}
            }
            return 0;
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 弹出右键菜单，返回被点中的命令（0 = 取消）。
    fn popup_menu(hwnd: isize) -> usize {
        unsafe {
            let menu = CreatePopupMenu();
            let toggle = wide(if window_visible() { "隐藏主窗口" } else { "显示主窗口" });
            let start = wide("启动 DeepSeek Harness");
            let stop = wide("关闭 DeepSeek Harness");
            let open = wide("在浏览器打开 Web UI");
            let quit = wide("退出（同时关闭 DSH）");
            AppendMenuW(menu, MF_STRING, CMD_TOGGLE, toggle.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, CMD_START, start.as_ptr());
            AppendMenuW(menu, MF_STRING, CMD_STOP, stop.as_ptr());
            AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

            // 「关闭按钮行为」子菜单：当前生效的那一项打勾（单选观感）
            let to_tray = super::close_to_tray();
            let sub = CreatePopupMenu();
            let opt_tray = wide("最小化到系统托盘（后台继续运行）");
            let opt_exit = wide("直接关闭启动器");
            let flag = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
            AppendMenuW(sub, flag(to_tray), CMD_CLOSE_TRAY, opt_tray.as_ptr());
            AppendMenuW(sub, flag(!to_tray), CMD_CLOSE_EXIT, opt_exit.as_ptr());
            let sub_title = wide("关闭按钮行为");
            AppendMenuW(menu, MF_POPUP, sub as usize, sub_title.as_ptr());

            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, CMD_QUIT, quit.as_ptr());

            let mut pt = std::mem::zeroed();
            let _ = GetCursorPos(&mut pt);
            // 必须置前，否则点菜单外部不会自动关闭
            let main = main_hwnd().unwrap_or(hwnd) as HWND;
            let _ = SetForegroundWindow(main);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                pt.x,
                pt.y,
                0,
                hwnd as HWND,
                std::ptr::null(),
            ) as usize;
            let _ = DestroyMenu(menu);
            cmd
        }
    }

    fn window_visible() -> bool {
        match main_hwnd() {
            Some(h) => unsafe { IsWindowVisible(h as HWND) != 0 },
            None => true,
        }
    }

    /// 显示/隐藏主窗口。
    pub fn set_window_visible(show: bool) {
        if let Some(h) = main_hwnd() {
            unsafe { ShowWindow(h as HWND, if show { SW_SHOW } else { SW_HIDE }) };
        }
    }

    pub fn is_window_visible() -> bool {
        window_visible()
    }

    /// 安装托盘图标。返回是否成功。
    pub fn install(ctx: &egui::Context) -> bool {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::Shell::{
            Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NOTIFYICONDATAW,
        };
        let _ = CTX.set(ctx.clone());
        unsafe {
            let class = wide("DSH_Launch_Console_TrayWnd");
            let hinst = GetModuleHandleW(std::ptr::null());
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = hinst;
            wc.lpszClassName = class.as_ptr();
            let _ = RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinst,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::config::log("WARNING: tray message window creation failed");
                return false;
            }
            let _ = HWND_SLOT.set(hwnd as isize);

            let icon = make_icon();
            if icon == 0 {
                crate::config::log("WARNING: tray icon creation failed");
                return false;
            }
            let _ = ICON.set(icon);

            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            nid.hIcon = icon as HICON;
            let tip = wide("DSH Launch Console");
            for (d, s) in nid.szTip.iter_mut().zip(tip) {
                *d = s;
            }
            if Shell_NotifyIconW(NIM_ADD, &nid) == 0 {
                crate::config::log("WARNING: Shell_NotifyIcon(NIM_ADD) failed");
                return false;
            }
            crate::config::log("tray icon installed");
            true
        }
    }

    /// 摘除托盘图标（退出前调用；Drop 不可靠，显式更好）。
    pub fn remove() {
        use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE, NOTIFYICONDATAW};
        unsafe {
            let (Some(hwnd), Some(icon)) = (HWND_SLOT.get().copied(), ICON.get().copied()) else {
                return;
            };
            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd as HWND;
            nid.uID = 1;
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            let _ = DestroyIcon(icon as HICON);
            crate::config::log("tray icon removed");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn available() -> bool {
        false
    }
    pub fn install(_ctx: &egui::Context) -> bool {
        false
    }
    pub fn remove() {}
    pub fn set_main_hwnd(_h: isize) {}
    pub fn set_window_visible(_show: bool) {}
    pub fn is_window_visible() -> bool {
        true
    }
}

pub use imp::*;

// dsh-launch-console — DSH Launch Console
//
// 纯启动器：用原生 GUI（egui，不依赖 WebView）管理 DeepSeek Harness 的
// 启动 / 关闭 / 版本 / 插件，DSH 的 Web UI 交给系统默认浏览器承载。
//
// 平台差异（同一套源码）：
//  - Windows: CREATE_NO_WINDOW 隐藏终端 + 命名作业对象整树清理 + 系统托盘
//  - Linux/macOS: 自成进程组 + killpg 整树清理（无托盘，关闭即退出）
#![cfg_attr(all(not(test), windows), windows_subsystem = "windows")]

mod app;
mod config;
mod dsh;
mod icon;
mod plugins;
mod procs;
mod selftest;
mod settings;
mod tray;
mod versions;
mod webui;

#[cfg(windows)]
mod winproc;

fn main() {
    // 诊断模式：不建窗口，只验证「启动 → 就绪 → 拿 token → 整树关闭」这条链路。
    // 用独立端口/独立 DSH_HOME，绝不碰正在使用的会话。
    if std::env::args().any(|a| a == "--selftest" || a == "--diagnose") {
        selftest::run();
        return;
    }

    // 单实例：已有实例在运行则直接退出（避免两个启动器抢同一棵 DSH 进程树）
    if !single_instance::acquire() {
        config::log("another instance is running; exiting");
        return;
    }

    #[cfg(windows)]
    {
        // 未打包应用缺少显式 AUMID 时，任务栏图标可能显示为空白
        unsafe {
            use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
            let id: Vec<u16> = "DSH.LaunchConsole"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let _ = SetCurrentProcessExplicitAppUserModelID(id.as_ptr());
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DSH Launch Console")
            .with_inner_size([1000.0, 730.0])
            .with_min_inner_size([880.0, 620.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "DSH Launch Console",
        options,
        Box::new(|cc| {
            // 外观（浅色主题 + 控件配色 + 字号）统一在 app 里设置
            Ok(Box::new(app::App::new(cc)))
        }),
    ) {
        config::log(&format!("eframe exited with error: {}", e));
        eprintln!("DSH Launch Console 启动失败: {}", e);
    }

    config::log("process exiting");
}

/// 单实例闸门。
mod single_instance {
    /// 尝试成为唯一实例；返回 false 表示已有实例在运行。
    #[cfg(windows)]
    pub fn acquire() -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        // 可用 DSH_LAUNCH_CONSOLE_MUTEX 覆盖，便于并存测试实例
        let name = crate::config::env_nonempty("DSH_LAUNCH_CONSOLE_MUTEX")
            .unwrap_or_else(|| "DSH_Launch_Console_SingleInstance_Mutex".to_string());
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 1, wide.as_ptr());
            if handle.is_null() {
                return true; // 创建失败也继续，不阻塞使用
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return false;
            }
            // 句柄故意不关闭：持有到进程结束，由系统回收
            true
        }
    }

    /// Unix：对临时目录里的 <名字>.lock 加 flock（不新建目录）。
    #[cfg(unix)]
    pub fn acquire() -> bool {
        use std::os::unix::io::AsRawFd;
        let name = crate::config::env_nonempty("DSH_LAUNCH_CONSOLE_MUTEX")
            .unwrap_or_else(|| "DSH_Launch_Console_SingleInstance".to_string());
        let path = crate::config::data_dir().join(format!("{}.lock", name));
        let Ok(file) = std::fs::OpenOptions::new().create(true).write(true).open(&path) else {
            return true;
        };
        let ok = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if ok {
            // 故意泄漏：锁由进程生命周期持有
            std::mem::forget(file);
        }
        ok
    }
}

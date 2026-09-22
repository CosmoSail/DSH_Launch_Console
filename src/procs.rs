//! 子进程启动助手：统一隐藏终端窗口。
//!
//! 0.2.0 需要在后台调用 npm / pnpm / dsh，这些都不该弹出黑框
//! （Windows 上 GUI 子系统的进程启动控制台程序会闪窗）。

use std::path::Path;
use std::process::{Command, Stdio};

use crate::trf;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 构造一个「不弹窗、不读 stdin」的 Command。
pub fn hidden_command(program: &Path) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW
        c.creation_flags(0x0800_0000);
    }
    c
}

/// 隐藏执行并等待，返回 (成功, 合并输出)。
pub fn run_capture(program: &Path, args: &[&str], cwd: Option<&Path>) -> Result<(bool, String), String> {
    let mut c = hidden_command(program);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().map_err(|e| trf!("执行 {} 失败: {}", program.display(), e))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&err);
    }
    Ok((out.status.success(), text))
}

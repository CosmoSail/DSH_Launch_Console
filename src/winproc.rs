#![allow(dead_code)] // 平台相关/预留 API
//! Windows 专属：作业对象整树清理 + 进程存活探测。
//!
//! 沿用 0.1.0 验证过的机制：`KILL_ON_JOB_CLOSE` 让内核在句柄关闭时
//! 一次性结束整棵进程树（含 DSH 拉起的 MCP server 等子进程），
//! 即使本进程崩溃/被杀也生效。

#![cfg(windows)]

use windows_sys::Win32::Foundation::HANDLE;

/// 持有作业对象句柄；Drop 时关闭句柄 → 内核清树。
pub struct KillJob {
    handle: HANDLE,
}

impl KillJob {
    fn create_internal() -> Option<Self> {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            };
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(handle);
                return None;
            }
            Some(Self { handle })
        }
    }

    fn assign(&self, pid: u32) -> bool {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            use windows_sys::Win32::System::Threading::{
                OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
            };
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return false;
            }
            let ok = AssignProcessToJobObject(self.handle, process) != 0;
            CloseHandle(process);
            ok
        }
    }

    pub fn kill_all(&self) {
        unsafe {
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            TerminateJobObject(self.handle, 1);
        }
    }
}

impl Drop for KillJob {
    fn drop(&mut self) {
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            CloseHandle(self.handle);
        }
    }
}

/// 把 pid 关进一个 KILL_ON_JOB_CLOSE 作业对象。
pub fn arm_job(pid: u32) -> Option<KillJob> {
    let job = KillJob::create_internal()?;
    if job.assign(pid) {
        crate::config::log(&format!("pid {} assigned to kill-on-close job", pid));
        Some(job)
    } else {
        crate::config::log(&format!("WARNING: failed to assign pid {} to job", pid));
        None
    }
}

/// 进程是否存活。
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}

//! Keeps the engine's whole process tree tied to the app.
//!
//! On Windows the venv's `python.exe` is a launcher that starts the real interpreter as its own
//! child, so killing the process we spawned would leave the engine running. The engine is started
//! suspended, put in a job object that kills every member when its last handle closes, then
//! resumed. Terminating the job stops the whole tree, and if the app itself dies (even killed
//! from Task Manager) Windows closes the handle and the engine goes with it.

use std::process::Child;

#[cfg(windows)]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    pub const CREATE_SUSPENDED: u32 = 0x0000_0004;

    pub struct Group(usize); // job HANDLE, stored as an integer so the struct is Send + Sync

    impl Group {
        /// Puts a process that was created suspended into a new kill-on-close job, then lets it run.
        pub fn adopt(child: &Child) -> Option<Group> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    resume_all(child.id());
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                let ok = AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) != 0;
                resume_all(child.id());
                if ok {
                    Some(Group(job as usize))
                } else {
                    CloseHandle(job);
                    None
                }
            }
        }

        pub fn kill(&self) {
            unsafe {
                TerminateJobObject(self.0 as HANDLE, 1);
            }
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }

    /// Resumes every thread of a process created with CREATE_SUSPENDED.
    unsafe fn resume_all(pid: u32) {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snap == INVALID_HANDLE_VALUE {
                return;
            }
            let mut entry: THREADENTRY32 = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
            if Thread32First(snap, &mut entry) != 0 {
                loop {
                    if entry.th32OwnerProcessID == pid {
                        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                        if !thread.is_null() {
                            ResumeThread(thread);
                            CloseHandle(thread);
                        }
                    }
                    if Thread32Next(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::process::Child;
    pub const CREATE_SUSPENDED: u32 = 0;
    pub struct Group(u32);
    impl Group {
        pub fn adopt(child: &Child) -> Option<Group> {
            Some(Group(child.id()))
        }
        pub fn kill(&self) {
            // The engine is started in its own process group (see engine.rs): signal all of it
            unsafe {
                libc_kill(-(self.0 as i32), 9);
            }
        }
    }
    unsafe extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
}

pub use imp::{Group, CREATE_SUSPENDED};

/// Convenience used by engine.rs.
pub fn adopt(child: &Child) -> Option<Group> {
    Group::adopt(child)
}

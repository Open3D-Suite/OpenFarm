// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Job Objects: a render host and every process it starts (3dsmaxbatch starts 3dsmax.exe,
//! Corona's DrServer starts its own Max) live in one job, so killing the job kills all of them, and
//! closing the agent closes them too. Phase 0 found orphaned Max processes without this.

use std::ffi::c_void;

use anyhow::Result;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};
use windows::core::PCWSTR;

pub struct ProcessGroup(HANDLE);

// SAFETY: a job handle is a kernel handle usable from any thread.
unsafe impl Send for ProcessGroup {}
unsafe impl Sync for ProcessGroup {}

impl ProcessGroup {
    pub fn new() -> Result<ProcessGroup> {
        // SAFETY: plain Win32 calls; the info struct outlives the call that reads it.
        unsafe {
            let job = CreateJobObjectW(None, PCWSTR::null())?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
            Ok(ProcessGroup(job))
        }
    }

    pub fn add(&self, pid: u32) -> Result<()> {
        // SAFETY: the process handle is closed right after use.
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid)?;
            let result = AssignProcessToJobObject(self.0, process);
            let _ = CloseHandle(process);
            result?;
        }
        Ok(())
    }

    pub fn kill(&self) {
        // SAFETY: terminating our own job.
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }

    /// The most memory the processes of the job used at once, in MB.
    pub fn peak_memory_mb(&self) -> Option<u64> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: the buffer is the size the call is told.
        unsafe {
            QueryInformationJobObject(
                Some(self.0),
                JobObjectExtendedLimitInformation,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )
            .ok()?;
        }
        Some(info.PeakJobMemoryUsed as u64 / (1024 * 1024))
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Closing the last handle kills every process in the job (KILL_ON_JOB_CLOSE).
        // SAFETY: we own the handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

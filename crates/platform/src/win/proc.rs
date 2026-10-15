//! Процессы: Toolhelp32, имя exe, время создания, package family, цепочка родителей.

use crate::pure::{file_name_of, is_system_process};
use anyhow::{Context, Result};
use callscribe_core::detection::rules::ProcessInfo;
use std::collections::{HashMap, HashSet};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, FILETIME, HANDLE};
use windows::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::core::PWSTR;

/// Запись снимка процессов.
#[derive(Debug, Clone)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub exe: String,
}

/// Открытый дескриптор процесса (закрывается при drop).
pub struct ProcHandle(HANDLE);

impl ProcHandle {
    pub fn open(pid: u32) -> Option<Self> {
        if pid == 0 {
            return None;
        }
        unsafe {
            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .ok()
                .map(Self)
        }
    }

    /// Время создания процесса (100-нс интервалы FILETIME).
    pub fn creation_time(&self) -> Option<u64> {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(self.0, &mut c, &mut e, &mut k, &mut u).ok()? };
        Some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }

    /// Имя файла exe (без каталога).
    pub fn image_name(&self) -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .ok()?
        };
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(file_name_of(&full).to_string())
    }

    /// Package family name упакованного приложения.
    pub fn package_family(&self) -> Option<String> {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        let rc = unsafe { GetPackageFamilyName(self.0, &mut len, Some(PWSTR(buf.as_mut_ptr()))) };
        if rc != ERROR_SUCCESS || len < 2 {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..(len - 1) as usize]))
    }
}

impl Drop for ProcHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Снимок всех процессов.
pub fn snapshot() -> Result<Vec<ProcEntry>> {
    let mut out = Vec::new();
    unsafe {
        let snap =
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).context("CreateToolhelp32Snapshot")?;
        let mut e = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = e
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(e.szExeFile.len());
            out.push(ProcEntry {
                pid: e.th32ProcessID,
                ppid: e.th32ParentProcessID,
                exe: String::from_utf16_lossy(&e.szExeFile[..n]),
            });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    Ok(out)
}

/// `ProcessInfo` по записи снимка: имя из `QueryFullProcessImageNameW`, запасное — из Toolhelp.
pub fn process_info(entry: &ProcEntry) -> ProcessInfo {
    let h = ProcHandle::open(entry.pid);
    let exe_name = h
        .as_ref()
        .and_then(|h| h.image_name())
        .unwrap_or_else(|| entry.exe.clone());
    ProcessInfo {
        pid: entry.pid,
        exe_name,
        package_family: h.and_then(|h| h.package_family()),
    }
}

/// Процесс и его предки (ближайший первым). Родитель берётся, только если он старше потомка
/// (защита от переиспользования PID); обрыв на системных процессах и при отсутствии родителя.
pub fn build_chain(pid: u32, snap: &[ProcEntry]) -> Vec<ProcessInfo> {
    let by_pid: HashMap<u32, &ProcEntry> = snap.iter().map(|e| (e.pid, e)).collect();
    let mut chain = Vec::new();
    let Some(mut cur) = by_pid.get(&pid).copied() else {
        return chain;
    };
    let mut seen = HashSet::new();
    seen.insert(cur.pid);
    chain.push(process_info(cur));
    while chain.len() < 16 {
        let Some(parent) = by_pid.get(&cur.ppid).copied() else {
            break;
        };
        if !seen.insert(parent.pid) || is_system_process(&parent.exe) {
            break;
        }
        let times = ProcHandle::open(cur.pid)
            .and_then(|h| h.creation_time())
            .zip(ProcHandle::open(parent.pid).and_then(|h| h.creation_time()));
        match times {
            Some((child_t, parent_t)) if parent_t < child_t => {}
            _ => break,
        }
        chain.push(process_info(parent));
        cur = parent;
    }
    chain
}

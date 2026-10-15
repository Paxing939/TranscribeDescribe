//! Источник «окна» (F-11, §11 п. 19): видимые top-level окна и запущенные процессы.

use super::proc::{ProcHandle, snapshot};
use anyhow::Result;
use callscribe_core::detection::rules::{ProcessInfo, WindowInfo};
use callscribe_core::platform_api::WindowProvider;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetWindowLongW, GetWindowPlacement, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    WINDOWPLACEMENT, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

const MIN_SIZE: i32 = 200;

struct RawWindow {
    pid: u32,
    title: String,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lparam.0 as *mut Vec<RawWindow>) };
    if let Some(w) = unsafe { inspect(hwnd) } {
        out.push(w);
    }
    BOOL(1)
}

/// Подходит ли окно: видимое, не cloaked, не tool-window, с заголовком и размером от 200x200.
unsafe fn inspect(hwnd: HWND) -> Option<RawWindow> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 && ex & WS_EX_APPWINDOW.0 == 0 {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if title.trim().is_empty() {
            return None;
        }
        // У свёрнутых окон реальный прямоугольник — «нормальное» положение.
        let rect = if IsIconic(hwnd).as_bool() {
            let mut wp = WINDOWPLACEMENT {
                length: size_of::<WINDOWPLACEMENT>() as u32,
                ..Default::default()
            };
            GetWindowPlacement(hwnd, &mut wp).ok()?;
            wp.rcNormalPosition
        } else {
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r).ok()?;
            r
        };
        if rect.right - rect.left < MIN_SIZE || rect.bottom - rect.top < MIN_SIZE {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (pid != 0).then_some(RawWindow { pid, title })
    }
}

/// Окна и процессы; имена exe кэшируются по PID вместе со временем старта процесса.
#[derive(Debug, Default)]
pub struct Win32WindowProvider {
    cache: HashMap<u32, (u64, String)>,
}

impl Win32WindowProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn exe_for(&mut self, pid: u32) -> Option<String> {
        let h = ProcHandle::open(pid)?;
        let start = h.creation_time().unwrap_or(0);
        if let Some((t, name)) = self.cache.get(&pid)
            && *t == start
        {
            return Some(name.clone());
        }
        let name = h.image_name()?;
        self.cache.insert(pid, (start, name.clone()));
        Some(name)
    }
}

impl WindowProvider for Win32WindowProvider {
    fn windows(&mut self) -> Result<Vec<WindowInfo>> {
        let mut raw: Vec<RawWindow> = Vec::new();
        unsafe {
            EnumWindows(
                Some(enum_cb),
                LPARAM(&mut raw as *mut Vec<RawWindow> as isize),
            )?
        };
        let live: HashSet<u32> = raw.iter().map(|w| w.pid).collect();
        self.cache.retain(|pid, _| live.contains(pid));
        let mut out = Vec::new();
        for w in raw {
            if let Some(exe_name) = self.exe_for(w.pid) {
                out.push(WindowInfo {
                    pid: w.pid,
                    exe_name,
                    title: w.title,
                });
            }
        }
        Ok(out)
    }

    fn processes(&mut self) -> Result<Vec<ProcessInfo>> {
        // package_family здесь не запрашиваем (дорого): только pid и имя из снимка.
        Ok(snapshot()?
            .into_iter()
            .filter(|e| e.pid != 0)
            .map(|e| ProcessInfo {
                pid: e.pid,
                exe_name: e.exe,
                package_family: None,
            })
            .collect())
    }
}

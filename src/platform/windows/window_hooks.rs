use crate::core::config::WindowGeometry;
use crate::modules::ModuleRegistry;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

static DROP_REGISTRY: OnceLock<Arc<Mutex<ModuleRegistry>>> = OnceLock::new();
static PREV_WNDPROC: AtomicIsize = AtomicIsize::new(0);

#[cfg(windows)]
pub fn extract_hwnd(window: &slint::Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = window.window_handle().window_handle() {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            return win32.hwnd.get();
        }
    }
    0
}

#[cfg(not(windows))]
pub fn extract_hwnd(_window: &slint::Window) -> isize {
    0
}

#[cfg(windows)]
pub fn bring_hwnd_to_front(hwnd_val: isize) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE,
    };
    if hwnd_val == 0 {
        return;
    }
    unsafe {
        let hwnd = hwnd_val as HWND;
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
    let _ = SW_HIDE;
}

#[cfg(not(windows))]
pub fn bring_hwnd_to_front(_hwnd_val: isize) {}

#[cfg(windows)]
pub fn hide_hwnd(hwnd_val: isize) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    if hwnd_val != 0 {
        unsafe {
            ShowWindow(hwnd_val as HWND, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
pub fn hide_hwnd(_hwnd_val: isize) {}

#[cfg(windows)]
unsafe extern "system" fn subclass_wndproc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{CallWindowProcW, WM_DROPFILES, WNDPROC};

    if msg == WM_DROPFILES {
        let hdrop = wparam as HDROP;
        let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
        let mut dropped_paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            if len > 0 {
                let mut buf = vec![0u16; (len + 1) as usize];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
                if let Ok(s) = String::from_utf16(&buf[..len as usize]) {
                    dropped_paths.push(PathBuf::from(s));
                }
            }
        }
        DragFinish(hdrop);
        if !dropped_paths.is_empty() {
            if let Some(reg_arc) = DROP_REGISTRY.get() {
                if let Ok(reg) = reg_arc.lock() {
                    reg.dispatch_dropped_files(&dropped_paths);
                }
            }
        }
        return 0;
    }

    let prev_ptr = PREV_WNDPROC.load(Ordering::SeqCst);
    let prev_proc: WNDPROC = std::mem::transmute(prev_ptr);
    CallWindowProcW(prev_proc, hwnd, msg, wparam, lparam)
}

pub fn install_win32_subclass(hwnd_val: isize, registry: Arc<Mutex<ModuleRegistry>>) {
    let _ = DROP_REGISTRY.set(registry);
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::UI::Shell::DragAcceptFiles;
        use windows_sys::Win32::UI::WindowsAndMessaging::{SetWindowLongPtrW, GWLP_WNDPROC};
        if hwnd_val == 0 {
            return;
        }
        unsafe {
            let hwnd = hwnd_val as HWND;
            DragAcceptFiles(hwnd, 1);
            let prev = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, subclass_wndproc as *const () as isize);
            PREV_WNDPROC.store(prev, Ordering::SeqCst);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_val;
    }
}

pub fn restore_window_geometry(hwnd_val: isize, geom: &WindowGeometry) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, MONITORINFO,
            MONITOR_DEFAULTTOPRIMARY, MONITOR_DEFAULTTONULL,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, ShowWindow, SWP_NOZORDER, SW_MAXIMIZE,
        };

        if hwnd_val == 0 || geom.width < 360 || geom.height < 480 {
            return;
        }
        let hwnd = hwnd_val as HWND;
        let mut x = geom.x;
        let mut y = geom.y;
        let mut w = geom.width.max(360);
        let mut h = geom.height.max(540);

        unsafe {
            let target_rect = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            let hmon = MonitorFromRect(&target_rect, MONITOR_DEFAULTTONULL);
            if hmon.is_null() {
                // Off-screen monitor disconnected: rescue onto primary work area
                let primary = MonitorFromPoint(
                    windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
                    MONITOR_DEFAULTTOPRIMARY,
                );
                let mut mi: MONITORINFO = std::mem::zeroed();
                mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if GetMonitorInfoW(primary, &mut mi) != 0 {
                    let work = mi.rcWork;
                    w = w.min((work.right - work.left).max(400));
                    h = h.min((work.bottom - work.top).max(480));
                    x = work.left + 40;
                    y = work.top + 40;
                } else {
                    x = 80;
                    y = 80;
                }
            }
            let _ = SetWindowPos(hwnd, std::ptr::null_mut(), x, y, w, h, SWP_NOZORDER);
            if geom.maximized {
                let _ = ShowWindow(hwnd, SW_MAXIMIZE);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (hwnd_val, geom);
    }
}

pub fn read_window_geometry(hwnd_val: isize) -> Option<WindowGeometry> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowPlacement, GetWindowRect, WINDOWPLACEMENT, SW_SHOWMAXIMIZED,
        };
        if hwnd_val == 0 {
            return None;
        }
        let hwnd = hwnd_val as HWND;
        unsafe {
            let mut wp: WINDOWPLACEMENT = std::mem::zeroed();
            wp.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            let maximized =
                GetWindowPlacement(hwnd, &mut wp) != 0 && wp.showCmd == SW_SHOWMAXIMIZED as u32;
            let r = if maximized {
                wp.rcNormalPosition
            } else {
                let mut rect: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut rect) == 0 {
                    return None;
                }
                rect
            };
            let width = (r.right - r.left).max(360);
            let height = (r.bottom - r.top).max(540);
            return Some(WindowGeometry {
                x: r.left,
                y: r.top,
                width,
                height,
                maximized,
            });
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd_val;
        None
    }
}

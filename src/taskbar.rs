//! Taskbar button progress (like Explorer's file copies) and flashing it when a run finishes
//! in the background.

use winapi::Interface;
use winapi::shared::windef::HWND;
use winapi::shared::wtypesbase::CLSCTX_INPROC_SERVER;
use winapi::um::combaseapi::CoCreateInstance;
use winapi::um::shobjidl_core::{
    CLSID_TaskbarList, ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL,
};
use winapi::um::winuser::{FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx, GetForegroundWindow};

pub struct Taskbar {
    hwnd: HWND,
    list: *mut ITaskbarList3,
}

impl Taskbar {
    /// Needs COM initialized on this thread (native-windows-gui does that).
    pub fn new(hwnd: HWND) -> Option<Taskbar> {
        let mut list: *mut ITaskbarList3 = std::ptr::null_mut();
        unsafe {
            let hr = CoCreateInstance(
                &CLSID_TaskbarList,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &ITaskbarList3::uuidof(),
                &mut list as *mut _ as *mut _,
            );
            if hr < 0 || list.is_null() {
                return None;
            }
            if (*list).HrInit() < 0 {
                (*list).Release();
                return None;
            }
        }
        Some(Taskbar { hwnd, list })
    }

    pub fn progress(&self, done: u64, total: u64) {
        unsafe {
            (*self.list).SetProgressState(self.hwnd, TBPF_NORMAL);
            (*self.list).SetProgressValue(self.hwnd, done, total.max(1));
        }
    }

    pub fn clear(&self) {
        unsafe { (*self.list).SetProgressState(self.hwnd, TBPF_NOPROGRESS) };
    }
}

impl Drop for Taskbar {
    fn drop(&mut self) {
        unsafe { (*self.list).Release() };
    }
}

/// Flash the taskbar button until the window is activated, if it isn't in front.
pub fn flash_if_background(hwnd: HWND) {
    unsafe {
        if GetForegroundWindow() == hwnd {
            return;
        }
        let mut info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        FlashWindowEx(&mut info);
    }
}

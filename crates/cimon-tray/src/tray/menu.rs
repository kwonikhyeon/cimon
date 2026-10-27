//! The popup menu (docs/tray.md §3.2), built from the last snapshot each time it opens.
//! Items this user cannot do without elevating carry the UAC shield.

use super::{poll::Snapshot, wide};
use crate::model::Probe;
use cimon_ctl::{Right, State};
use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, DeleteDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP};
use windows_sys::Win32::UI::Shell::{SHGetStockIconInfo, SHGSI_ICON, SHGSI_SMALLICON, SHSTOCKICONINFO, SIID_SHIELD};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const ID_WEB: usize = 10;
pub const ID_START: usize = 20;
pub const ID_STOP_IDLE: usize = 21;
pub const ID_STOP_NOW: usize = 22;
pub const ID_CANCEL_DRAIN: usize = 23;
pub const ID_RESTART: usize = 24;
pub const ID_QUIT: usize = 99;

/// The UAC shield as a menu bitmap (32-bit with alpha).
pub fn shield() -> HBITMAP {
    // SAFETY: GDI objects made and released here; the bitmap is kept for the process
    unsafe {
        let mut info: SHSTOCKICONINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHSTOCKICONINFO>() as u32;
        if SHGetStockIconInfo(SIID_SHIELD, SHGSI_ICON | SHGSI_SMALLICON, &mut info) != 0 || info.hIcon.is_null() {
            return std::ptr::null_mut();
        }
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if !bmp.is_null() {
            let old = SelectObject(dc, bmp);
            DrawIconEx(dc, 0, 0, info.hIcon, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
            SelectObject(dc, old);
        }
        DeleteDC(dc);
        DestroyIcon(info.hIcon);
        bmp
    }
}

unsafe fn add(menu: HMENU, flags: u32, id: usize, text: &str) {
    let w = wide(text);
    AppendMenuW(menu, flags, id, w.as_ptr());
}

unsafe fn mark_shield(menu: HMENU, id: usize, bmp: HBITMAP) {
    if bmp.is_null() {
        return;
    }
    let mut mii: MENUITEMINFOW = std::mem::zeroed();
    mii.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
    mii.fMask = MIIM_BITMAP;
    mii.hbmpItem = bmp;
    SetMenuItemInfoW(menu, id as u32, 0, &mii);
}

/// (running, queued, stopping when idle) from the last probe.
pub fn jobs(snap: &Snapshot) -> (u64, u64, bool) {
    match &snap.probe {
        Some(Probe::Answered(h)) => (h.running, h.queued, h.stopping_when_idle),
        _ => (0, 0, false),
    }
}

pub fn build(snap: &Snapshot, tip: &str, shield: HBITMAP) -> HMENU {
    // SAFETY: a new menu filled here; the caller destroys it
    unsafe {
        let menu = CreatePopupMenu();
        add(menu, MF_STRING | MF_GRAYED, 0, tip);
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add(menu, MF_STRING, ID_WEB, "웹 화면 열기");
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let may = cimon_ctl::may(Right::StartStop);
        match snap.svc {
            State::Stopped => {
                add(menu, MF_STRING, ID_START, "시작");
                if !may {
                    mark_shield(menu, ID_START, shield);
                }
            }
            State::Running => {
                let (running, _, _) = jobs(snap);
                add(menu, MF_STRING, ID_STOP_NOW, if running > 0 { "지금 중지 — 실행 중인 작업은 중단된다" } else { "중지" });
                add(menu, MF_STRING, ID_RESTART, "재시작");
                if !may {
                    mark_shield(menu, ID_STOP_NOW, shield);
                    mark_shield(menu, ID_RESTART, shield);
                }
            }
            State::NotInstalled => add(menu, MF_STRING | MF_GRAYED, 0, "서비스가 설치되지 않았다(cimon setup)"),
            _ => add(menu, MF_STRING | MF_GRAYED, 0, "시작·중지하는 중…"),
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        add(menu, MF_STRING, ID_QUIT, "트레이 끝내기");
        menu
    }
}

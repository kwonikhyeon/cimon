//! The notification-area icon: drawn at run time (a colored disc per state, sized for
//! the display), registered under a fixed GUID so Windows remembers where the user put
//! it — out of the hidden-icons overflow or not (docs/tray.md §3.4).

use crate::model::Color;
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, CreateDIBSection, DeleteObject, GetDC, ReleaseDC, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_GUID, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW,
    NOTIFYICON_VERSION_4,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON};

/// The tray's identity for the shell. Tied to the executable's path by Windows, which is
/// why the tray is meant to run from `C:\Program Files\cimon\` only; from anywhere else
/// the GUID is refused and the icon falls back to a window-relative id.
const GUID_ITEM: GUID = GUID::from_u128(0x6f1d_2c4e_8a3b_4d57_9c21_5e7a_0b3c_1d9e);

pub struct Icon {
    hwnd: HWND,
    use_guid: bool,
    icons: Vec<(Color, HICON)>,
    color: Color,
    tip: String,
}

fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Green => (0x2e, 0x9d, 0x4e),
        Color::Blue => (0x2f, 0x6f, 0xdb),
        Color::Yellow => (0xe0, 0xa8, 0x00),
        Color::Gray => (0x8a, 0x8f, 0x98),
        Color::Red => (0xd6, 0x45, 0x45),
    }
}

/// A disc with a darker rim, anti-aliased, as a 32-bit icon with alpha.
fn draw(c: Color) -> HICON {
    // SAFETY: plain GDI object creation; every object made here is deleted here
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: size, biHeight: -size, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dc = GetDC(std::ptr::null_mut());
        let color = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        ReleaseDC(std::ptr::null_mut(), dc);
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        let (r, g, b) = rgb(c);
        let f = size as f32;
        let center = (f - 1.0) / 2.0;
        let radius = f * 0.40;
        let rim = (f / 16.0).max(1.0);
        for y in 0..size {
            for x in 0..size {
                let d = ((x as f32 - center).powi(2) + (y as f32 - center).powi(2)).sqrt();
                let a = (radius + 0.5 - d).clamp(0.0, 1.0);
                let shade = if d > radius - rim { 0.68 } else { 1.0 };
                let (pr, pg, pb) = ((r as f32 * shade * a) as u32, (g as f32 * shade * a) as u32, (b as f32 * shade * a) as u32);
                px[(y * size + x) as usize] = ((a * 255.0) as u32) << 24 | pr << 16 | pg << 8 | pb;
            }
        }
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let h = CreateIconIndirect(&ii);
        DeleteObject(mask);
        DeleteObject(color);
        h
    }
}

/// Whether this is the installed tray (`%ProgramFiles%\cimon\cimon-tray.exe`).
fn installed() -> bool {
    let (Ok(exe), Some(pf)) = (std::env::current_exe(), std::env::var_os("ProgramFiles")) else { return false };
    let want = std::path::PathBuf::from(pf).join("cimon");
    exe.parent().is_some_and(|d| d.to_string_lossy().eq_ignore_ascii_case(&want.to_string_lossy()))
}

fn copy_into(dst: &mut [u16], s: &str) {
    let w: Vec<u16> = s.encode_utf16().take(dst.len() - 1).collect();
    dst[..w.len()].copy_from_slice(&w);
    dst[w.len()] = 0;
}

impl Icon {
    fn data(&self) -> NOTIFYICONDATAW {
        // SAFETY: a zeroed NOTIFYICONDATAW is a valid starting point
        let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        d.hWnd = self.hwnd;
        d.uID = 1;
        if self.use_guid {
            d.uFlags |= NIF_GUID;
            d.guidItem = GUID_ITEM;
        }
        d
    }

    fn handle(&self, c: Color) -> HICON {
        self.icons.iter().find(|(k, _)| *k == c).map(|(_, h)| *h).unwrap_or(std::ptr::null_mut())
    }

    fn add_now(&mut self) -> bool {
        // Windows ties the GUID to the first path that uses it: a run from a build folder
        // must not claim it for the installed tray
        let tries: &[bool] = if installed() { &[true, false] } else { &[false] };
        for &use_guid in tries {
            self.use_guid = use_guid;
            let mut d = self.data();
            d.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
            d.uCallbackMessage = super::WM_TRAY;
            d.hIcon = self.handle(self.color);
            copy_into(&mut d.szTip, &self.tip);
            // SAFETY: d is fully initialized for NIM_ADD
            if unsafe { Shell_NotifyIconW(NIM_ADD, &d) } != 0 {
                let mut v = self.data();
                v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                unsafe { Shell_NotifyIconW(NIM_SETVERSION, &v) };
                super::log::line(&format!("icon added ({})", if use_guid { "guid" } else { "window id" }));
                return true;
            }
            // a GUID registered to another path (or a stale entry) is refused — and an
            // icon left over from a crashed run blocks the add: clear it and retry
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
            if unsafe { Shell_NotifyIconW(NIM_ADD, &d) } != 0 {
                let mut v = self.data();
                v.Anonymous.uVersion = NOTIFYICON_VERSION_4;
                unsafe { Shell_NotifyIconW(NIM_SETVERSION, &v) };
                super::log::line(&format!("icon added after clearing a stale one ({})", if use_guid { "guid" } else { "window id" }));
                return true;
            }
        }
        super::log::line("icon could not be added");
        false
    }

    pub fn add(hwnd: HWND, color: Color, tip: &str) -> Icon {
        let icons = [Color::Green, Color::Blue, Color::Yellow, Color::Gray, Color::Red].into_iter().map(|c| (c, draw(c))).collect();
        let mut i = Icon { hwnd, use_guid: false, icons, color, tip: tip.to_string() };
        i.add_now();
        i
    }

    /// After Explorer restarts (`TaskbarCreated`) the icon is gone: add it again.
    pub fn readd(&mut self) {
        self.add_now();
    }

    pub fn update(&mut self, color: Color, tip: &str) {
        if color == self.color && tip == self.tip {
            return;
        }
        self.color = color;
        self.tip = tip.to_string();
        super::log::line(&format!("state: {tip}"));
        let mut d = self.data();
        d.uFlags |= NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.hIcon = self.handle(color);
        copy_into(&mut d.szTip, tip);
        // SAFETY: d identifies the icon added above
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    /// A balloon (a toast on Windows 11).
    pub fn notify(&self, title: &str, text: &str, warning: bool) {
        let mut d = self.data();
        d.uFlags |= NIF_INFO;
        d.dwInfoFlags = if warning { NIIF_WARNING } else { NIIF_INFO };
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
    }

    pub fn remove(&self) {
        unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) };
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        for (_, h) in &self.icons {
            if !h.is_null() {
                unsafe { DestroyIcon(*h) };
            }
        }
    }
}

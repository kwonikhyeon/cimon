//! The Win32 side: one hidden window owning the icon and the menu, a status thread, and
//! short worker threads for anything that blocks (HTTP, service control).

mod actions;
mod elevate;
mod icon;
mod log;
mod menu;
mod poll;
mod settings;
mod token;
mod web;

use crate::model;
use std::cell::RefCell;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_STATUS: u32 = WM_APP + 2;
pub const WM_NOTICE: u32 = WM_APP + 3;

const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Balloons from worker threads: (title, text, warning).
type Notices = Arc<Mutex<Vec<(String, String, bool)>>>;

struct Tray {
    icon: icon::Icon,
    shield: windows_sys::Win32::Graphics::Gdi::HBITMAP,
    shared: Arc<Mutex<poll::Snapshot>>,
    poke: Sender<()>,
    taskbar_created: u32,
    notices: Notices,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

pub fn run() {
    // SAFETY: plain Win32 calls on this thread; the window and icon live until the loop ends
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // one tray per session
        let mutex = wide("Local\\cimon-tray");
        let m = CreateMutexW(std::ptr::null(), 1, mutex.as_ptr());
        if m.is_null() || GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        log::line(&format!("start {}", env!("CARGO_PKG_VERSION")));
        let class = wide("cimon-tray");
        let hinst = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&wc);
        // a hidden ordinary window: a message-only one would not hear TaskbarCreated
        let title = wide("cimon");
        let hwnd = CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPED, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null());
        if hwnd.is_null() {
            log::line("window could not be created");
            return;
        }
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        let shared = Arc::new(Mutex::new(poll::Snapshot::default()));
        let (poke, rx) = std::sync::mpsc::channel();
        let first = model::view(cimon_ctl::query().unwrap_or(cimon_ctl::State::Other), None);
        let icon = icon::Icon::add(hwnd, first.color, &first.tip);
        TRAY.with(|t| *t.borrow_mut() = Some(Tray { icon, shield: menu::shield(), shared: shared.clone(), poke, taskbar_created, notices: Default::default() }));
        poll::spawn(hwnd as usize, shared, rx);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        TRAY.with(|t| {
            if let Some(t) = t.borrow_mut().take() {
                t.icon.remove();
            }
        });
        log::line("exit");
    }
}

/// Runs `f` on the tray state (UI thread only).
fn with_tray<R>(f: impl FnOnce(&mut Tray) -> R) -> Option<R> {
    TRAY.with(|t| t.borrow_mut().as_mut().map(f))
}

/// A balloon from a worker thread.
fn post_notice(hwnd: usize, notices: &Notices, title: &str, text: &str, warning: bool) {
    notices.lock().unwrap_or_else(|p| p.into_inner()).push((title.to_string(), text.to_string(), warning));
    // SAFETY: posting to the tray window
    unsafe { PostMessageW(hwnd as _, WM_NOTICE, 0, 0) };
}

fn refresh(t: &mut Tray) {
    let snap = t.shared.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let v = model::view(snap.svc, snap.probe.as_ref());
    t.icon.update(v.color, &v.tip);
}

unsafe fn show_menu(hwnd: HWND) {
    let Some((menu, snap)) = with_tray(|t| {
        let snap = t.shared.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let tip = model::view(snap.svc, snap.probe.as_ref()).tip;
        (menu::build(&snap, &tip, t.shield), snap)
    }) else {
        return;
    };
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    // the menu closes when the user clicks elsewhere only if this window is foreground
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN, pt.x, pt.y, hwnd, std::ptr::null()) as usize;
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);
    command(hwnd, cmd, &snap);
}

/// Asks before something that cuts running jobs short.
fn confirm(hwnd: HWND, text: &str) -> bool {
    let (t, c) = (wide(text), wide("cimon"));
    // SAFETY: NUL-terminated strings, owner window is the tray's
    unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND) == IDYES }
}

/// A service action on a worker thread; its answer comes back as a balloon.
fn act(hwnd: HWND, a: actions::Action) {
    let Some((notices, poke)) = with_tray(|t| (t.notices.clone(), t.poke.clone())) else { return };
    let h = hwnd as usize;
    std::thread::spawn(move || {
        match actions::perform(a) {
            Ok(Some(msg)) => post_notice(h, &notices, "cimon", &msg, false),
            Ok(None) => {}
            Err(e) => post_notice(h, &notices, "cimon 을 제어하지 못했다", &e, true),
        }
        let _ = poke.send(());
    });
}

/// An admin command (docs/tray.md §5): always through UAC, in its own window.
enum Admin {
    Cimon(&'static [&'static str]),
    PowerShell,
}

fn admin(hwnd: HWND, what: Admin) {
    let Some(notices) = with_tray(|t| t.notices.clone()) else { return };
    let h = hwnd as usize;
    std::thread::spawn(move || {
        let r = match what {
            Admin::Cimon(args) => elevate::cimon(args),
            Admin::PowerShell => elevate::powershell(),
        };
        if let Err(e) = r {
            post_notice(h, &notices, "관리자 명령을 실행하지 못했다", &e, true);
        }
    });
}

fn command(hwnd: HWND, cmd: usize, snap: &poll::Snapshot) {
    let (running, queued, _) = menu::jobs(snap);
    match cmd {
        menu::ID_WEB => {
            let Some(notices) = with_tray(|t| t.notices.clone()) else { return };
            let h = hwnd as usize;
            std::thread::spawn(move || match web::login_link() {
                Ok(link) => web::open(&link),
                Err(e) => post_notice(h, &notices, "웹 화면을 열지 못했다", &e, true),
            });
        }
        menu::ID_START => act(hwnd, actions::Action::Start),
        menu::ID_STOP_NOW | menu::ID_RESTART => {
            let restart = cmd == menu::ID_RESTART;
            if running > 0 && !confirm(hwnd, &format!("실행 중인 작업 {running}개가 중단된다(interrupted, 대기 {queued}개는 다음 시작 때 돈다). 지금 {}할까?", if restart { "재시작" } else { "중지" })) {
                return;
            }
            act(hwnd, if restart { actions::Action::Restart } else { actions::Action::StopNow });
        }
        menu::ID_INVITE => admin(hwnd, Admin::Cimon(&["invite", "--ask"])),
        menu::ID_PROVISION => admin(hwnd, Admin::Cimon(&["provision", "--ask"])),
        menu::ID_DOCTOR => admin(hwnd, Admin::Cimon(&["doctor"])),
        menu::ID_CONFIG => admin(hwnd, Admin::Cimon(&["edit-config"])),
        menu::ID_POWERSHELL => admin(hwnd, Admin::PowerShell),
        menu::ID_UPGRADE => {
            let Some(notices) = with_tray(|t| t.notices.clone()) else { return };
            post_notice(hwnd as usize, &notices, "cimon", "새 버전 올리기는 아직 없다", true);
        }
        menu::ID_STOP_IDLE => act(hwnd, actions::Action::DrainStop),
        menu::ID_CANCEL_DRAIN => act(hwnd, actions::Action::CancelDrain),
        menu::ID_QUIT => {
            // SAFETY: ends the message loop
            unsafe { DestroyWindow(hwnd) };
        }
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lp as u32) & 0xffff {
                WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_STATUS => {
            with_tray(refresh);
            0
        }
        WM_NOTICE => {
            with_tray(|t| {
                let pending: Vec<_> = std::mem::take(&mut *t.notices.lock().unwrap_or_else(|p| p.into_inner()));
                for (title, text, warning) in pending {
                    t.icon.notify(&title, &text, warning);
                }
            });
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => {
            if with_tray(|t| t.taskbar_created != 0 && msg == t.taskbar_created) == Some(true) {
                log::line("explorer restarted — adding the icon again");
                with_tray(|t| {
                    t.icon.readd();
                    let _ = t.poke.send(());
                });
                return 0;
            }
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

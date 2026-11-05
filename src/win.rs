//! Win32 placement in physical pixels: monitors, outer-corner defaults, moving windows,
//! native right-click menu (an egui menu would be clipped to the tiny window).

use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn new(l: i32, t: i32, w: i32, h: i32) -> Self {
        Rect { l, t, r: l + w, b: t + h }
    }
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
    fn cx(&self) -> i32 {
        (self.l + self.r) / 2
    }
    fn cy(&self) -> i32 {
        (self.t + self.b) / 2
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
}

impl From<RECT> for Rect {
    fn from(r: RECT) -> Self {
        Rect { l: r.left, t: r.top, r: r.right, b: r.bottom }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub work: Rect,
    pub primary: bool,
    pub scale: f32,
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = unsafe { &mut *(data as *mut Vec<Monitor>) };
        let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
        mi.cbSize = size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(h, &mut mi) } != 0 {
            let (mut dx, mut dy) = (96u32, 96u32);
            unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
            out.push(Monitor { work: mi.rcWork.into(), primary: mi.dwFlags & 1 != 0, scale: dx as f32 / 96.0 });
        }
        1
    }
    let mut v: Vec<Monitor> = Vec::new();
    unsafe { EnumDisplayMonitors(null_mut(), null(), Some(cb), &mut v as *mut _ as LPARAM) };
    v
}

/// Key for saved positions: docked and undocked setups each keep their own spots.
pub fn signature(ms: &[Monitor]) -> String {
    let mut parts: Vec<String> = ms.iter().map(|m| format!("{},{},{},{}@{}", m.work.l, m.work.t, m.work.r, m.work.b, m.scale)).collect();
    parts.sort();
    parts.join(";")
}

/// Monitor indexes in default-placement order: primary first, then left to right.
pub fn order(ms: &[Monitor]) -> Vec<usize> {
    let mut o: Vec<usize> = (0..ms.len()).collect();
    o.sort_by_key(|&i| (!ms[i].primary, ms[i].work.l));
    o
}

/// Monitor holding the rect's centre, else the nearest one.
pub fn monitor_of(ms: &[Monitor], r: Rect) -> usize {
    let d = |m: &Monitor| {
        let dx = (r.cx() - r.cx().clamp(m.work.l, m.work.r - 1)) as i64;
        let dy = (r.cy() - r.cy().clamp(m.work.t, m.work.b - 1)) as i64;
        dx * dx + dy * dy
    };
    (0..ms.len()).min_by_key(|&i| d(&ms[i])).unwrap_or(0)
}

/// Outer top corner of monitor `m`: top-left when it sits left of the desktop centre, else top-right.
pub fn corner(ms: &[Monitor], m: usize, w: i32, h: i32) -> Rect {
    let desk_l = ms.iter().map(|x| x.work.l).min().unwrap_or(0);
    let desk_r = ms.iter().map(|x| x.work.r).max().unwrap_or(0);
    let mon = ms[m].work;
    let gap = (8.0 * ms[m].scale).round() as i32;
    let l = if mon.cx() < (desk_l + desk_r) / 2 { mon.l + gap } else { mon.r - gap - w };
    Rect::new(l, mon.t + gap, w, h)
}

/// Resize keeping the edges nearest the monitor's edges fixed (a top-right widget grows left
/// and down), then keep it on the monitor.
pub fn anchored(old: Rect, w: i32, h: i32, mon: Rect) -> Rect {
    let l = if old.cx() > mon.cx() { old.r - w } else { old.l };
    let t = if old.cy() > mon.cy() { old.b - h } else { old.t };
    Rect::new(l.clamp(mon.l, (mon.r - w).max(mon.l)), t.clamp(mon.t, (mon.b - h).max(mon.t)), w, h)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn find(title: &str) -> Option<HWND> {
    let h = unsafe { FindWindowW(null(), wide(title).as_ptr()) };
    (!h.is_null()).then_some(h)
}

pub fn rect(h: HWND) -> Rect {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(h, &mut r) };
    r.into()
}

pub fn place(h: HWND, r: Rect) {
    unsafe { SetWindowPos(h, HWND_TOPMOST, r.l, r.t, r.w(), r.h(), SWP_NOACTIVATE) };
}

/// Tool window: no taskbar button, not in Alt-Tab.
pub fn hide_from_alt_tab(h: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW as isize);
    }
}

/// Native popup at the cursor. Item id 0 = separator. Returns the picked id, 0 if dismissed.
pub fn menu(h: HWND, items: &[(u32, &str)]) -> u32 {
    unsafe {
        let m = CreatePopupMenu();
        for &(id, text) in items {
            if id == 0 {
                AppendMenuW(m, MF_SEPARATOR, 0, null());
            } else {
                AppendMenuW(m, MF_STRING, id as usize, wide(text).as_ptr());
            }
        }
        let mut p = POINT { x: 0, y: 0 };
        GetCursorPos(&mut p);
        SetForegroundWindow(h); // without this the menu does not close on click-away
        let id = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, h, null());
        DestroyMenu(m);
        id as u32
    }
}

// ---------- tray icon ----------

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_ID: u32 = 1;
const QUIT: u32 = 1;

static ON_QUIT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
/// Id of the "TaskbarCreated" broadcast, sent when Explorer restarts (the icon is gone by then).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Tray icon with a right-click Quit. A hidden window of its own receives the icon's messages,
/// so winit's window is left alone. Removes the icon on drop.
pub struct Tray(HWND);

impl Tray {
    /// `on_quit` runs on the UI thread when Quit is picked.
    pub fn new(on_quit: impl Fn() + Send + Sync + 'static) -> Option<Tray> {
        let _ = ON_QUIT.set(Box::new(on_quit));
        let class: &'static [u16] = Box::leak(wide("usage-widget-tray").into_boxed_slice());
        unsafe {
            let inst = GetModuleHandleW(null());
            let wc = WNDCLASSW { lpfnWndProc: Some(tray_proc), hInstance: inst, lpszClassName: class.as_ptr(), ..std::mem::zeroed() };
            RegisterClassW(&wc);
            TASKBAR_CREATED.store(RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()), Ordering::Relaxed);
            let h = CreateWindowExW(WS_EX_TOOLWINDOW, class.as_ptr(), wide("usage-widget tray").as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), inst, null());
            if h.is_null() {
                crate::usage::log("tray: window not created, no tray icon");
                return None;
            }
            if !add_icon(h) {
                // Usually Explorer is not up yet at logon; the TaskbarCreated broadcast adds it later.
                crate::usage::log("tray: icon not added yet, waiting for the taskbar");
            }
            Some(Tray(h))
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(self.0)) };
    }
}

fn icon_data(h: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = h;
    nid.uID = TRAY_ID;
    nid
}

fn add_icon(h: HWND) -> bool {
    let mut nid = icon_data(h);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    for (dst, src) in nid.szTip.iter_mut().zip(wide("Claude usage widget")) {
        *dst = src;
    }
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) != 0 }
}

unsafe extern "system" fn tray_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TRAY && lp as u32 == WM_RBUTTONUP {
        if menu(h, &[(QUIT, "Quit usage widget")]) == QUIT
            && let Some(quit) = ON_QUIT.get()
        {
            quit();
        }
        return 0;
    }
    let taskbar = TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar != 0 && msg == taskbar {
        add_icon(h);
    }
    unsafe { DefWindowProcW(h, msg, wp, lp) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(l: i32, w: i32, primary: bool) -> Monitor {
        Monitor { work: Rect::new(l, 0, w, 1080), primary, scale: 1.0 }
    }

    #[test]
    fn outer_corners_left_monitor_top_left_others_top_right() {
        // Three monitors: left, primary in the middle, right
        let ms = [mon(1920, 1920, false), mon(0, 1920, true), mon(-1920, 1920, false)];
        assert_eq!(order(&ms), vec![1, 2, 0]);
        assert_eq!(corner(&ms, 2, 190, 44), Rect::new(-1920 + 8, 8, 190, 44));
        assert_eq!(corner(&ms, 0, 190, 44), Rect::new(3840 - 8 - 190, 8, 190, 44));
        assert_eq!(corner(&ms, 1, 190, 44), Rect::new(1920 - 8 - 190, 8, 190, 44));
        // single monitor: top-right
        assert_eq!(corner(&[mon(0, 1920, true)], 0, 190, 44).l, 1920 - 8 - 190);
    }

    #[test]
    fn grows_away_from_the_corner_and_stays_on_screen() {
        let m = Rect::new(0, 0, 1920, 1080);
        let top_right = Rect::new(1722, 8, 190, 44);
        assert_eq!(anchored(top_right, 300, 276, m), Rect::new(1612, 8, 300, 276));
        let top_left = Rect::new(8, 8, 190, 44);
        assert_eq!(anchored(top_left, 300, 276, m), Rect::new(8, 8, 300, 276));
        let bottom_right = Rect::new(1722, 1028, 190, 44);
        assert_eq!(anchored(bottom_right, 300, 276, m), Rect::new(1612, 796, 300, 276));
        // collapse returns to the same compact spot
        assert_eq!(anchored(Rect::new(1612, 8, 300, 276), 190, 44, m), top_right);
    }

    #[test]
    fn monitor_of_picks_containing_else_nearest() {
        let ms = [mon(0, 1920, true), mon(1920, 1920, false)];
        assert_eq!(monitor_of(&ms, Rect::new(2000, 10, 100, 40)), 1);
        assert_eq!(monitor_of(&ms, Rect::new(5000, 10, 100, 40)), 1);
        assert_eq!(monitor_of(&ms, Rect::new(-500, 10, 100, 40)), 0);
    }
}

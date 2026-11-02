//! Lists the connected screens.

use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

#[derive(Clone, Debug)]
pub struct Screen {
    pub device: String,
    pub rect: RECT,
    pub work: RECT,
    pub primary: bool,
    pub dpi: u32,
}

impl Screen {
    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }
    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }
    pub fn label(&self, n: usize) -> String {
        let mut s = format!("Screen {n}  ·  {}×{}", self.width(), self.height());
        if self.height() > self.width() {
            s.push_str("  portrait");
        }
        if self.primary {
            s.push_str("  (main)");
        }
        s
    }
}

unsafe extern "system" fn enum_cb(hmon: HMONITOR, _hdc: HDC, _r: *mut RECT, data: LPARAM) -> BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<Screen>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
        let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        out.push(Screen {
            device: String::from_utf16_lossy(&info.szDevice[..len]),
            rect: info.monitorInfo.rcMonitor,
            work: info.monitorInfo.rcWork,
            primary: info.monitorInfo.dwFlags & 1 != 0,
            dpi: dx.max(48),
        });
    }
    BOOL(1)
}

/// Screens ordered left to right.
pub fn list() -> Vec<Screen> {
    let mut v: Vec<Screen> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(&mut v as *mut _ as isize));
    }
    v.sort_by_key(|s| (s.rect.left, s.rect.top));
    v
}

/// The chosen screen, or the default: the first non-main screen (the main one is for work),
/// falling back to the main screen.
pub fn pick<'a>(screens: &'a [Screen], wanted: Option<&str>) -> Option<&'a Screen> {
    if let Some(w) = wanted {
        if let Some(s) = screens.iter().find(|s| s.device == w) {
            return Some(s);
        }
    }
    screens.iter().find(|s| !s.primary).or_else(|| screens.first())
}

#![windows_subsystem = "windows"]
//! AI Widget: a small desktop panel showing what AI agents are running on this PC.
//!
//! Flow: every refresh (2 s by default) a timer fires → read the process list (one system
//! call) → group processes under the agent that started them → redraw only if something
//! visible changed. Nothing runs between timer ticks.

mod agents;
mod config;
mod describe;
mod hooklog;
mod procs;
mod render;
mod screens;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, IDLE_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling,
    SetPriorityClass, SetProcessInformation,
};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use agents::{InfoCache, Session};
use config::{Config, Corner};
use render::{Finished, Renderer, View};

const TIMER_ID: usize = 1;
const MARGIN_DIP: f32 = 16.0;

static ON_TOP: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static DEBUG: AtomicBool = AtomicBool::new(false);

/// Set AIWIDGET_DEBUG=1 to log timings to %TEMP%iwidget-debug.log.
fn debug_log(msg: &str) {
    if !DEBUG.load(Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let path = std::env::temp_dir().join("aiwidget-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

struct Done {
    title: String,
    ended: Instant,
}

struct App {
    hwnd: HWND,
    cfg: Config,
    sampler: procs::Sampler,
    cache: InfoCache,
    hooks: hooklog::HookLog,
    renderer: Renderer,
    sessions: Vec<Session>,
    finished: HashMap<u32, Vec<Done>>,
    self_pid: u32,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

impl App {
    fn tick(&mut self) {
        let t0 = Instant::now();
        let list = self.sampler.sample();
        let t_sample = t0.elapsed();
        let alive = self.cache.retain_live(&list);
        self.hooks.refresh();
        let sessions = agents::build_sessions(&list, &mut self.cache, &self.hooks.map, self.self_pid);

        // Remember tasks that just ended so they can linger briefly as "done".
        let now = Instant::now();
        for old in &self.sessions {
            for t in &old.tasks {
                if t.is_finished(&alive) {
                    let v = self.finished.entry(old.root_pid).or_default();
                    v.insert(0, Done { title: t.title.clone(), ended: now });
                    v.truncate(3);
                }
            }
        }
        let live: HashSet<u32> = sessions.iter().map(|s| s.root_pid).collect();
        self.finished.retain(|k, v| {
            v.retain(|d| now.duration_since(d.ended).as_secs() < 20);
            live.contains(k) && !v.is_empty()
        });
        self.sessions = sessions;
        let t_build = t0.elapsed();
        self.draw(false);
        if DEBUG.load(Ordering::Relaxed) {
            let mut r = RECT::default();
            unsafe {
                let _ = GetWindowRect(self.hwnd, &mut r);
            }
            debug_log(&format!(
                "tick: {} procs, sample {:.2} ms, build {:.2} ms, total {:.2} ms, {} sessions, window {},{} {}x{}",
                list.len(),
                t_sample.as_secs_f64() * 1e3,
                t_build.as_secs_f64() * 1e3,
                t0.elapsed().as_secs_f64() * 1e3,
                self.sessions.len(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top
            ));
            for s in &self.sessions {
                debug_log(&format!("  {:?} pid {} {:?} host {:?} cpu {:.1} mem {} tasks {} mcps {}", s.kind, s.root_pid, s.project, s.host, s.cpu, s.mem / 1048576, s.tasks.len(), s.mcps.len()));
                for t in &s.tasks {
                    debug_log(&format!("     - {} | {}", t.title, t.detail.chars().take(100).collect::<String>()));
                }
            }
        }
    }

    fn draw(&mut self, force: bool) {
        let screens = screens::list();
        let Some(screen) = screens::pick(&screens, self.cfg.monitor.as_deref()).cloned() else { return };
        let scale = screen.dpi as f32 / 96.0 * self.cfg.scale;
        let work_h = (screen.work.bottom - screen.work.top) as f32 / scale - 2.0 * MARGIN_DIP;

        let show_finished = self.cfg.show_finished;
        let finished = &self.finished;
        let now = Instant::now();
        let fin = move |root: u32| -> Vec<Finished> {
            if !show_finished {
                return Vec::new();
            }
            finished
                .get(&root)
                .map(|v| v.iter().map(|d| Finished { title: d.title.clone(), ago_s: now.duration_since(d.ended).as_secs() }).collect())
                .unwrap_or_default()
        };

        // Fit on screen: show fewer tasks per agent if it would be too tall.
        let mut max_tasks = 6;
        let view = loop {
            let v = View { sessions: &self.sessions, finished: &fin, max_tasks };
            if max_tasks <= 1 || self.renderer.measure(&v) <= work_h {
                break v;
            }
            max_tasks -= 1;
        };

        let w_px = (render::WIDTH_DIP * scale).ceil() as i32;
        let h_px = (self.renderer.measure(&view) * scale).ceil() as i32;
        let pos = if DRAGGING.load(Ordering::Relaxed) { None } else { Some(place(&self.cfg, &screen, w_px, h_px, scale)) };
        self.renderer.draw(self.hwnd, &view, scale, pos, self.cfg.glow, self.cfg.opacity, force);
    }

    fn apply_layer(&self) {
        ON_TOP.store(self.cfg.on_top, Ordering::Relaxed);
        let after = if self.cfg.on_top { HWND_TOPMOST } else { HWND_BOTTOM };
        unsafe {
            if !self.cfg.on_top {
                let _ = SetWindowPos(self.hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            let _ = SetWindowPos(self.hwnd, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }

    fn restart_timer(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ID, self.cfg.refresh_ms.clamp(500, 60_000), None);
        }
    }
}

/// Top-left position in pixels for the widget on `screen`.
fn place(cfg: &Config, screen: &screens::Screen, w: i32, h: i32, scale: f32) -> (i32, i32) {
    let wk = screen.work;
    let m = (MARGIN_DIP * scale) as i32;
    let (x, y) = if let Some((dx, dy)) = cfg.custom {
        (wk.left + dx, wk.top + dy)
    } else {
        match cfg.corner {
            Corner::TopLeft => (wk.left + m, wk.top + m),
            Corner::TopRight => (wk.right - m - w, wk.top + m),
            Corner::BottomLeft => (wk.left + m, wk.bottom - m - h),
            Corner::BottomRight => (wk.right - m - w, wk.bottom - m - h),
        }
    };
    // Keep it fully on the screen.
    (x.clamp(wk.left, (wk.right - w).max(wk.left)), y.clamp(wk.top, (wk.bottom - h).max(wk.top)))
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

// Menu command ids.
const ID_SCREEN: usize = 100;
const ID_CORNER: usize = 200;
const ID_LAYER_BEHIND: usize = 300;
const ID_LAYER_TOP: usize = 301;
const ID_SIZE: usize = 400;
const ID_REFRESH: usize = 500;
const ID_FINISHED: usize = 600;
const ID_GLOW: usize = 601;
const ID_FOLDER: usize = 700;
const ID_QUIT: usize = 999;

const SIZES: [(f32, &str); 3] = [(0.85, "Small"), (1.0, "Normal"), (1.2, "Large")];
const REFRESHES: [(u32, &str); 3] = [(1000, "Every second"), (2000, "Every 2 seconds"), (5000, "Every 5 seconds")];
const CORNERS: [(Corner, &str); 4] = [
    (Corner::TopLeft, "Top left"),
    (Corner::TopRight, "Top right"),
    (Corner::BottomLeft, "Bottom left"),
    (Corner::BottomRight, "Bottom right"),
];

fn add(menu: HMENU, id: usize, label: &str, checked: bool) {
    let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
    unsafe {
        let _ = AppendMenuW(menu, flags, id, &HSTRING::from(label));
    }
}

fn show_menu(hwnd: HWND) {
    // Build the menu while holding the app, then release it: the menu runs its own
    // message loop, which keeps delivering timer ticks.
    let Some((menu, screens)) = with_app(|app| unsafe {
        let screens = screens::list();
        let current = screens::pick(&screens, app.cfg.monitor.as_deref()).map(|s| s.device.clone());
        let menu = CreatePopupMenu().ok()?;

        let sm = CreatePopupMenu().ok()?;
        for (i, s) in screens.iter().enumerate() {
            add(sm, ID_SCREEN + i, &s.label(i + 1), Some(&s.device) == current.as_ref());
        }
        let _ = AppendMenuW(menu, MF_POPUP, sm.0 as usize, w!("Screen"));

        let cm = CreatePopupMenu().ok()?;
        for (i, (c, label)) in CORNERS.iter().enumerate() {
            add(cm, ID_CORNER + i, label, app.cfg.custom.is_none() && app.cfg.corner == *c);
        }
        let _ = AppendMenuW(menu, MF_POPUP, cm.0 as usize, w!("Corner  (or drag the widget)"));

        let lm = CreatePopupMenu().ok()?;
        add(lm, ID_LAYER_BEHIND, "Behind windows (on the desktop)", !app.cfg.on_top);
        add(lm, ID_LAYER_TOP, "Always on top", app.cfg.on_top);
        let _ = AppendMenuW(menu, MF_POPUP, lm.0 as usize, w!("Layer"));

        let zm = CreatePopupMenu().ok()?;
        for (i, (s, label)) in SIZES.iter().enumerate() {
            add(zm, ID_SIZE + i, label, (app.cfg.scale - s).abs() < 0.01);
        }
        let _ = AppendMenuW(menu, MF_POPUP, zm.0 as usize, w!("Size"));

        let rm = CreatePopupMenu().ok()?;
        for (i, (ms, label)) in REFRESHES.iter().enumerate() {
            add(rm, ID_REFRESH + i, label, app.cfg.refresh_ms == *ms);
        }
        let _ = AppendMenuW(menu, MF_POPUP, rm.0 as usize, w!("Refresh"));

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, ID_GLOW, "Glow background", app.cfg.glow);
        add(menu, ID_FINISHED, "Show recently finished", app.cfg.show_finished);
        add(menu, ID_FOLDER, "Open settings folder", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, ID_QUIT, "Close widget", false);
        Some((menu, screens))
    })
    .flatten() else {
        return;
    };

    let cmd = unsafe {
        let mut pt = Default::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let c = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        c.0 as usize
    };
    if cmd == 0 {
        return;
    }
    if cmd == ID_QUIT {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    if cmd == ID_FOLDER {
        let dir = config::dir();
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(dir.as_os_str()), None, None, SW_SHOWNORMAL);
        }
        return;
    }
    with_app(|app| {
        match cmd {
            c if (ID_SCREEN..ID_SCREEN + 50).contains(&c) => {
                if let Some(s) = screens.get(c - ID_SCREEN) {
                    app.cfg.monitor = Some(s.device.clone());
                    app.cfg.custom = None;
                }
            }
            c if (ID_CORNER..ID_CORNER + 4).contains(&c) => {
                app.cfg.corner = CORNERS[c - ID_CORNER].0;
                app.cfg.custom = None;
            }
            ID_LAYER_BEHIND => app.cfg.on_top = false,
            ID_LAYER_TOP => app.cfg.on_top = true,
            c if (ID_SIZE..ID_SIZE + 3).contains(&c) => app.cfg.scale = SIZES[c - ID_SIZE].0,
            c if (ID_REFRESH..ID_REFRESH + 3).contains(&c) => {
                app.cfg.refresh_ms = REFRESHES[c - ID_REFRESH].0;
                app.restart_timer();
            }
            ID_GLOW => app.cfg.glow = !app.cfg.glow,
            ID_FINISHED => app.cfg.show_finished = !app.cfg.show_finished,
            _ => {}
        }
        app.cfg.save();
        app.apply_layer();
        app.draw(true);
    });
}

/// After a drag, remember the spot relative to whichever screen it was dropped on.
fn finish_drag(hwnd: HWND) {
    with_app(|app| unsafe {
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let screens = screens::list();
        let cx = (r.left + r.right) / 2;
        let cy = (r.top + r.bottom) / 2;
        let s = screens
            .iter()
            .find(|s| cx >= s.rect.left && cx < s.rect.right && cy >= s.rect.top && cy < s.rect.bottom)
            .or_else(|| screens::pick(&screens, app.cfg.monitor.as_deref()));
        if let Some(s) = s {
            app.cfg.monitor = Some(s.device.clone());
            app.cfg.custom = Some((r.left - s.work.left, r.top - s.work.top));
            app.cfg.save();
        }
        app.draw(true);
    });
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                with_app(|app| app.tick());
                LRESULT(0)
            }
            // Whole widget is a drag handle.
            WM_NCHITTEST => LRESULT(HTCAPTION as isize),
            WM_NCLBUTTONDBLCLK => LRESULT(0),
            WM_NCRBUTTONUP | WM_CONTEXTMENU => {
                show_menu(hwnd);
                LRESULT(0)
            }
            WM_NCRBUTTONDOWN => LRESULT(0),
            WM_ENTERSIZEMOVE => {
                DRAGGING.store(true, Ordering::Relaxed);
                LRESULT(0)
            }
            WM_EXITSIZEMOVE => {
                DRAGGING.store(false, Ordering::Relaxed);
                finish_drag(hwnd);
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGING => {
                // "Behind windows" mode: refuse to come forward when clicked.
                if !ON_TOP.load(Ordering::Relaxed) {
                    let wp = &mut *(lp.0 as *mut WINDOWPOS);
                    wp.hwndInsertAfter = HWND_BOTTOM;
                    wp.flags &= !SWP_NOZORDER;
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
                with_app(|app| app.draw(true));
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// `--snapshot out.bmp`: draw one frame over a desktop-blue backdrop and save it, without
/// showing a window. Used to check the look.
fn write_snapshot(app: &App, path: &str) {
    let Some((px, w, h)) = app.renderer.pixels() else { return };
    let (pad, bw, bh) = (24i32, w + 48, h + 48);
    let mut out = Vec::with_capacity((54 + bw * bh * 4) as usize);
    let size = 54 + (bw * bh * 4) as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
    out.extend_from_slice(&bw.to_le_bytes());
    out.extend_from_slice(&(-bh).to_le_bytes());
    out.extend_from_slice(&[1, 0, 32, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&((bw * bh * 4) as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    for y in 0..bh {
        for x in 0..bw {
            // backdrop: a soft blue gradient like a wallpaper
            let t = y as f32 / bh as f32;
            let bg = [(70.0 + 40.0 * t) as u32, (60.0 + 20.0 * t) as u32, (40.0 + 10.0 * t) as u32]; // b,g,r
            let (sx, sy) = (x - pad, y - pad);
            let (b, g, r) = if sx >= 0 && sy >= 0 && sx < w && sy < h {
                let p = px[(sy * w + sx) as usize];
                let a = p >> 24;
                let ia = 255 - a;
                ((p & 255) + bg[0] * ia / 255, ((p >> 8) & 255) + bg[1] * ia / 255, ((p >> 16) & 255) + bg[2] * ia / 255)
            } else {
                (bg[0], bg[1], bg[2])
            };
            out.extend_from_slice(&[b.min(255) as u8, g.min(255) as u8, r.min(255) as u8, 255]);
        }
    }
    let _ = std::fs::write(path, out);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let snapshot = args.iter().position(|a| a == "--snapshot").and_then(|i| args.get(i + 1).cloned());
    DEBUG.store(std::env::var_os("AIWIDGET_DEBUG").is_some(), Ordering::Relaxed);
    unsafe {
        // One copy only.
        // (Snapshot runs skip this so they work while the widget is open.)
        if snapshot.is_none() {
            let _mutex = CreateMutexW(None, true, w!("Local\\aiwidget-single-instance"));
            if GetLastError() == ERROR_ALREADY_EXISTS {
                return;
            }
        }

        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // Efficiency mode (green leaf in Task Manager): lowest priority + EcoQoS.
        let _ = SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS);
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );

        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("AIWidgetWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&wc);

        let cfg = Config::load();
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        if cfg.on_top {
            ex |= WS_EX_TOPMOST;
        }
        let Ok(hwnd) = CreateWindowExW(ex, class, w!("AI Widget"), WS_POPUP, 0, 0, 10, 10, None, None, Some(hinst.into()), None) else {
            return;
        };

        let renderer = match Renderer::new() {
            Ok(r) => r,
            Err(_) => return,
        };
        let app = App {
            hwnd,
            cfg,
            sampler: procs::Sampler::new(),
            cache: InfoCache::default(),
            hooks: hooklog::HookLog::default(),
            renderer,
            sessions: Vec::new(),
            finished: HashMap::new(),
            self_pid: std::process::id(),
        };
        APP.with(|a| *a.borrow_mut() = Some(app));

        if let Some(path) = snapshot {
            with_app(|app| {
                app.tick();
                std::thread::sleep(std::time::Duration::from_millis(1500));
                app.tick();
                app.draw(true);
                write_snapshot(app, &path);
            });
            return;
        }

        with_app(|app| {
            app.tick();
            app.apply_layer();
            app.restart_timer();
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|a| a.borrow_mut().take());
    }
}

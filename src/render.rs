//! Draws the widget into a see-through bitmap and hands it to Windows.
//!
//! No GPU and no animation loop: we only draw when the numbers change (at most once per
//! refresh). The background is a tiny CPU "shader": wispy smoke lit by a soft glow per
//! agent that brightens with that agent's CPU use. It is computed on a coarse grid
//! (one sample per 4 points) and blended up, so a full redraw costs a few milliseconds.
//! Text is Direct2D + DirectWrite in software mode.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use windows::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1DCRenderTarget,
    ID2D1Factory, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::HSTRING;

use crate::agents::Session;

pub const WIDTH_DIP: f32 = 350.0;
const PAD: f32 = 12.0;
const RADIUS: f32 = 14.0;
const GRID: f32 = 4.0; // background shading grid, in DIPs

const TEXT: [f32; 4] = [0.93, 0.94, 0.96, 1.0];
const DIM: [f32; 4] = [0.66, 0.69, 0.76, 1.0];
const FAINT: [f32; 4] = [0.48, 0.51, 0.58, 1.0];

// CPU and memory icons. Sizes are whole DIPs so they stay crisp at 100% scaling.
const ICON_LINE: [f32; 4] = [0.50, 0.53, 0.60, 1.0];
const ICON_BG: [f32; 4] = [0.05, 0.06, 0.08, 1.0];
const ICON_OFF: [f32; 4] = [1.0, 1.0, 1.0, 0.13];
const ICON_NEUTRAL: [f32; 3] = [0.80, 0.83, 0.90];
const CHIP_W: f32 = 13.0;
const STICK_W: f32 = 15.0;
const CPU_W: f32 = 25.0;
const MEM_W: f32 = 41.0;
const ICON_GAP: f32 = 2.0;
const COL_GAP: f32 = 5.0;
const STATS_W: f32 = CHIP_W + ICON_GAP + CPU_W + COL_GAP + STICK_W + ICON_GAP + MEM_W;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Font {
    Header,
    HeaderR,
    Name,
    Body,
    Small,
    SmallR,
}

enum Op {
    Text { s: String, font: Font, r: [f32; 4], c: [f32; 4] },
    Rect { r: [f32; 4], radius: f32, c: [f32; 4] },
}

pub struct Light {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub color: [f32; 3],
    pub strength: f32,
}

pub struct Finished {
    pub title: String,
    pub ago_s: u64,
}

pub struct View<'a> {
    pub sessions: &'a [Session],
    pub finished: &'a dyn Fn(u32) -> Vec<Finished>,
    pub max_tasks: usize,
}

pub struct Renderer {
    _d2d: ID2D1Factory,
    dw: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    fonts: Vec<(Font, IDWriteTextFormat)>,
    memdc: HDC,
    dib: Option<(HBITMAP, HGDIOBJ, *mut u32, i32, i32)>,
    last_hash: u64,
    pub smoke_t: f32,
}

fn fmt_mem(b: u64) -> String {
    let mb = b as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{:.0} MB", mb) }
}

fn fmt_cpu(c: f32) -> String {
    if c < 0.5 { "0%".into() } else if c < 10.0 { format!("{:.1}%", c) } else { format!("{:.0}%", c) }
}

pub fn fmt_age(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

fn fmt_uptime(s: u64) -> String {
    if s < 60 {
        "just started".into()
    } else if s < 3600 {
        format!("up {}m", s / 60)
    } else {
        format!("up {}h {:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// Icon fill, 0 to 4. Stepped, not proportional: on a 24-thread PC one flat-out core is only
/// about 4%, so a straight scale would leave every icon looking empty. Exact numbers sit
/// beside the icons. Steps also mean an icon only changes when its number has changed too,
/// so the icons never cause an extra redraw.
fn cpu_level(cpu: f32) -> usize {
    [0.5, 4.0, 15.0, 40.0].iter().filter(|&&t| cpu >= t).count()
}

fn mem_level(mem: u64) -> usize {
    const MB: u64 = 1024 * 1024;
    [100 * MB, 500 * MB, 2048 * MB, 8192 * MB].iter().filter(|&&t| mem >= t).count()
}

fn rect(ops: &mut Vec<Op>, r: [f32; 4], radius: f32, c: [f32; 4]) {
    ops.push(Op::Rect { r, radius, c });
}

/// A chip with pins, filling from the bottom. 13 x 13 DIPs.
fn chip_icon(ops: &mut Vec<Op>, x: f32, y: f32, level: usize, color: [f32; 3]) {
    for i in [4.0, 8.0] {
        rect(ops, [x, y + i, CHIP_W, 1.0], 0.0, ICON_LINE);
        rect(ops, [x + i, y, 1.0, CHIP_W], 0.0, ICON_LINE);
    }
    rect(ops, [x + 2.0, y + 2.0, 9.0, 9.0], 1.5, ICON_LINE);
    rect(ops, [x + 3.0, y + 3.0, 7.0, 7.0], 0.0, ICON_BG);
    if level > 0 {
        let h = 7.0 * level as f32 / 4.0;
        rect(ops, [x + 3.0, y + 10.0 - h, 7.0, h], 0.0, [color[0], color[1], color[2], 1.0]);
    }
}

/// A memory stick whose four chips light up left to right. 15 x 10 DIPs.
fn stick_icon(ops: &mut Vec<Op>, x: f32, y: f32, level: usize, color: [f32; 3]) {
    rect(ops, [x + 1.0, y + 8.0, 6.0, 2.0], 0.0, ICON_LINE);
    rect(ops, [x + 8.0, y + 8.0, 6.0, 2.0], 0.0, ICON_LINE);
    rect(ops, [x, y, STICK_W, 8.0], 1.0, ICON_LINE);
    rect(ops, [x + 1.0, y + 1.0, 13.0, 6.0], 0.0, ICON_BG);
    for i in 0..4 {
        let c = if i < level { [color[0], color[1], color[2], 1.0] } else { ICON_OFF };
        rect(ops, [x + 2.0 + 3.0 * i as f32, y + 2.0, 2.0, 4.0], 0.0, c);
    }
}

/// Memory icon and number, ending at `right`. Returns the x where they start.
fn mem_stat(ops: &mut Vec<Op>, right: f32, y: f32, mem: u64, color: [f32; 3], tc: [f32; 4]) -> f32 {
    let text_x = right - MEM_W;
    let icon_x = text_x - ICON_GAP - STICK_W;
    stick_icon(ops, icon_x, y + 2.0, mem_level(mem), color);
    ops.push(Op::Text { s: fmt_mem(mem), font: Font::Small, r: [text_x, y, MEM_W, 15.0], c: tc });
    icon_x
}

/// CPU and memory, icon then number, in fixed columns so they line up down the panel.
/// Returns the x where they start.
fn stats(ops: &mut Vec<Op>, right: f32, y: f32, cpu: f32, mem: u64, color: [f32; 3], tc: [f32; 4]) -> f32 {
    let text_x = mem_stat(ops, right, y, mem, color, tc) - COL_GAP - CPU_W;
    let icon_x = text_x - ICON_GAP - CHIP_W;
    chip_icon(ops, icon_x, y + 1.0, cpu_level(cpu), color);
    ops.push(Op::Text { s: fmt_cpu(cpu), font: Font::Small, r: [text_x, y, CPU_W, 15.0], c: tc });
    icon_x
}

impl Renderer {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = d2d.CreateDCRenderTarget(&props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            let memdc = CreateCompatibleDC(None);
            let mut r = Self {
                _d2d: d2d,
                dw,
                rt,
                brush,
                fonts: Vec::new(),
                memdc,
                dib: None,
                last_hash: 0,
                smoke_t: 0.0,
            };
            r.make_fonts()?;
            Ok(r)
        }
    }

    fn make_fonts(&mut self) -> windows::core::Result<()> {
        let mk = |family: &str, weight: DWRITE_FONT_WEIGHT, size: f32, align: DWRITE_TEXT_ALIGNMENT| -> windows::core::Result<IDWriteTextFormat> {
            unsafe {
                let f = self.dw.CreateTextFormat(
                    &HSTRING::from(family),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    &HSTRING::from("en-us"),
                )?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                let sign = self.dw.CreateEllipsisTrimmingSign(&f)?;
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                f.SetTrimming(&trim, &sign)?;
                f.SetTextAlignment(align)?;
                Ok(f)
            }
        };
        let ui = "Segoe UI";
        self.fonts = vec![
            (Font::Header, mk(ui, DWRITE_FONT_WEIGHT_SEMI_BOLD, 11.0, DWRITE_TEXT_ALIGNMENT_LEADING)?),
            (Font::HeaderR, mk(ui, DWRITE_FONT_WEIGHT_NORMAL, 11.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?),
            (Font::Name, mk(ui, DWRITE_FONT_WEIGHT_BOLD, 13.0, DWRITE_TEXT_ALIGNMENT_LEADING)?),
            (Font::Body, mk(ui, DWRITE_FONT_WEIGHT_NORMAL, 12.5, DWRITE_TEXT_ALIGNMENT_LEADING)?),
            (Font::Small, mk(ui, DWRITE_FONT_WEIGHT_NORMAL, 10.5, DWRITE_TEXT_ALIGNMENT_LEADING)?),
            (Font::SmallR, mk(ui, DWRITE_FONT_WEIGHT_NORMAL, 10.5, DWRITE_TEXT_ALIGNMENT_TRAILING)?),
        ];
        Ok(())
    }

    fn font(&self, f: Font) -> &IDWriteTextFormat {
        &self.fonts.iter().find(|(k, _)| *k == f).unwrap().1
    }

    fn text_width(&self, s: &str, f: Font) -> f32 {
        let w: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(layout) = self.dw.CreateTextLayout(&w, self.font(f), 1000.0, 50.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }

    /// Lay out the content. Returns draw ops, glow lights and height (all in DIPs).
    fn layout(&self, v: &View) -> (Vec<Op>, Vec<Light>, f32) {
        let w = WIDTH_DIP;
        let mut ops = Vec::new();
        let mut lights = Vec::new();
        let mut y = 10.0;
        let inner_x = PAD + 22.0;
        let right_edge = w - PAD - 10.0;

        let total_cpu: f32 = v.sessions.iter().map(|s| s.cpu).sum();
        let total_mem: u64 = v.sessions.iter().map(|s| s.mem).sum();
        ops.push(Op::Text { s: "AI AGENTS".into(), font: Font::Header, r: [PAD + 4.0, y, 120.0, 16.0], c: DIM });
        if !v.sessions.is_empty() {
            let left = stats(&mut ops, right_edge, y + 1.0, total_cpu, total_mem, ICON_NEUTRAL, DIM);
            let running = format!("{} running", v.sessions.len());
            ops.push(Op::Text { s: running, font: Font::HeaderR, r: [left - 86.0, y, 80.0, 16.0], c: FAINT });
        }
        y += 24.0;

        if v.sessions.is_empty() {
            ops.push(Op::Text { s: "No AI agents running".into(), font: Font::Body, r: [PAD + 4.0, y, w - 2.0 * PAD, 18.0], c: FAINT });
            y += 26.0;
        }

        for s in v.sessions {
            let color = s.kind.color();
            let card_at = ops.len();
            let top = y;

            // Header: dot, agent name, project, totals.
            y += 6.0;
            ops.push(Op::Rect { r: [PAD + 7.0, y + 6.0, 8.0, 8.0], radius: 4.0, c: [color[0], color[1], color[2], 1.0] });
            let name = s.kind.label().to_string();
            let nw = self.text_width(&name, Font::Name);
            ops.push(Op::Text { s: name, font: Font::Name, r: [inner_x, y, nw + 2.0, 19.0], c: TEXT });
            if let Some(p) = &s.project {
                let px = inner_x + nw + 7.0;
                ops.push(Op::Text { s: p.clone(), font: Font::Body, r: [px, y + 0.5, (right_edge - STATS_W - 8.0 - px).max(0.0), 19.0], c: DIM });
            }
            stats(&mut ops, right_edge, y + 3.0, s.cpu, s.mem, color, DIM);
            lights.push(Light {
                x: PAD + 11.0,
                y: y + 10.0,
                radius: 150.0,
                color,
                strength: 0.6 + 0.9 * (s.cpu / 25.0).min(1.0),
            });
            y += 20.0;

            let mut sub = Vec::new();
            if let Some(h) = s.host {
                sub.push(format!("in {h}"));
            }
            if let Some(via) = s.via {
                sub.push(format!("started by {via}"));
            }
            sub.push(fmt_uptime(s.age_s));
            ops.push(Op::Text { s: sub.join("  ·  "), font: Font::Small, r: [inner_x, y, right_edge - inner_x, 14.0], c: FAINT });
            y += 16.0;

            if !s.mcps.is_empty() {
                let names: Vec<&str> = s.mcps.iter().map(|m| m.name.as_str()).collect();
                let mem: u64 = s.mcps.iter().map(|m| m.mem).sum();
                let left = mem_stat(&mut ops, right_edge, y, mem, color, FAINT);
                ops.push(Op::Text {
                    s: format!("MCP servers: {}", names.join(", ")),
                    font: Font::Small,
                    r: [inner_x, y, left - inner_x - 6.0, 14.0],
                    c: FAINT,
                });
                y += 16.0;
            }

            let shown = s.tasks.len().min(v.max_tasks);
            for t in &s.tasks[..shown] {
                y += 4.0;
                let busy = (t.cpu / 20.0).min(1.0);
                ops.push(Op::Rect {
                    r: [PAD + 9.5, y + 2.0, 3.0, 14.0],
                    radius: 1.5,
                    c: [color[0], color[1], color[2], 0.25 + 0.75 * busy],
                });
                if t.cpu > 1.0 {
                    lights.push(Light { x: PAD + 11.0, y: y + 9.0, radius: 60.0, color, strength: 0.4 + 1.0 * busy });
                }
                let mut left = stats(&mut ops, right_edge, y + 2.0, t.cpu, t.mem, color, DIM);
                if t.io > 512.0 * 1024.0 {
                    let io = format!("{:.1} MB/s", t.io / (1024.0 * 1024.0));
                    let iow = self.text_width(&io, Font::SmallR) + 4.0;
                    left -= iow + COL_GAP;
                    ops.push(Op::Text { s: io, font: Font::SmallR, r: [left, y + 2.0, iow, 15.0], c: DIM });
                }
                let age = fmt_age(t.age_s);
                let aw = self.text_width(&age, Font::SmallR) + 4.0;
                left -= aw + COL_GAP;
                ops.push(Op::Text { s: age, font: Font::SmallR, r: [left, y + 2.0, aw, 15.0], c: FAINT });
                // Summary only. The full command stays in the debug log, never on the panel.
                ops.push(Op::Text { s: t.title.clone(), font: Font::Body, r: [inner_x, y, left - inner_x - 6.0, 18.0], c: TEXT });
                y += 20.0;
                // What it's running underneath, by program name only.
                if !t.chain.is_empty() {
                    ops.push(Op::Text { s: format!("↳ {}", t.chain), font: Font::Small, r: [inner_x + 8.0, y - 2.0, right_edge - inner_x - 8.0, 14.0], c: FAINT });
                    y += 14.0;
                }
            }
            if s.tasks.len() > shown {
                ops.push(Op::Text {
                    s: format!("+ {} more", s.tasks.len() - shown),
                    font: Font::Small,
                    r: [inner_x, y + 2.0, 200.0, 14.0],
                    c: FAINT,
                });
                y += 18.0;
            }
            for f in (v.finished)(s.root_pid).iter().take(3) {
                let ago = format!("done {} ago", fmt_age(f.ago_s));
                let aw = self.text_width(&ago, Font::SmallR) + 4.0;
                ops.push(Op::Text { s: format!("✓  {}", f.title), font: Font::Small, r: [inner_x, y + 2.0, right_edge - inner_x - aw - 6.0, 14.0], c: FAINT });
                ops.push(Op::Text { s: ago, font: Font::SmallR, r: [right_edge - aw, y + 2.0, aw, 14.0], c: FAINT });
                y += 17.0;
            }
            y += 8.0;
            ops.insert(card_at, Op::Rect { r: [PAD, top, w - 2.0 * PAD, y - top], radius: 10.0, c: [1.0, 1.0, 1.0, 0.035] });
            y += 6.0;
        }
        y += 2.0;
        (ops, lights, y)
    }

    /// How tall the content would be, in DIPs.
    pub fn measure(&self, v: &View) -> f32 {
        self.layout(v).2
    }

    /// Draw and show. `pos` = top-left on screen in pixels, None to leave it where it is
    /// (while the user is dragging). Returns the size in pixels.
    pub fn draw(
        &mut self,
        hwnd: HWND,
        v: &View,
        scale: f32,
        pos: Option<(i32, i32)>,
        glow: bool,
        opacity: f32,
        force: bool,
    ) -> (i32, i32) {
        let (ops, lights, h_dip) = self.layout(v);
        let w_px = (WIDTH_DIP * scale).ceil() as i32;
        let h_px = (h_dip * scale).ceil() as i32;

        // Skip the redraw if nothing visible changed.
        let mut hasher = DefaultHasher::new();
        (w_px, h_px, pos, glow, (opacity * 100.0) as i32).hash(&mut hasher);
        for op in &ops {
            match op {
                Op::Text { s, r, .. } => (s, r.map(|x| x as i32)).hash(&mut hasher),
                Op::Rect { r, c, .. } => (r.map(|x| x as i32), c.map(|x| (x * 50.0) as i32)).hash(&mut hasher),
            }
        }
        let hash = hasher.finish();
        if !force && hash == self.last_hash {
            return (w_px, h_px);
        }
        self.last_hash = hash;
        self.smoke_t += 0.015; // the smoke drifts a little between redraws

        unsafe {
            // (Re)create the bitmap if the size changed.
            if self.dib.map(|d| (d.3, d.4)) != Some((w_px, h_px)) {
                if let Some((bmp, old, _, _, _)) = self.dib.take() {
                    SelectObject(self.memdc, old);
                    let _ = DeleteObject(bmp.into());
                }
                let bi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: w_px,
                        biHeight: -h_px,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
                let Ok(bmp) = CreateDIBSection(Some(self.memdc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                    return (w_px, h_px);
                };
                let old = SelectObject(self.memdc, bmp.into());
                self.dib = Some((bmp, old, bits as *mut u32, w_px, h_px));
            }
            let (_, _, bits, _, _) = self.dib.unwrap();
            let px = std::slice::from_raw_parts_mut(bits, (w_px * h_px) as usize);
            shade_background(px, w_px, h_px, scale, &lights, glow, opacity, self.smoke_t);

            // Text and shapes on top.
            let rc = RECT { left: 0, top: 0, right: w_px, bottom: h_px };
            if self.rt.BindDC(self.memdc, &rc).is_ok() {
                self.rt.SetDpi(96.0 * scale, 96.0 * scale);
                self.rt.BeginDraw();
                for op in &ops {
                    match op {
                        Op::Rect { r, radius, c } => {
                            self.brush.SetColor(&D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] });
                            let rect = D2D_RECT_F { left: r[0], top: r[1], right: r[0] + r[2], bottom: r[1] + r[3] };
                            if *radius > 0.0 {
                                let rr = D2D1_ROUNDED_RECT { rect, radiusX: *radius, radiusY: *radius };
                                self.rt.FillRoundedRectangle(&rr, &self.brush);
                            } else {
                                self.rt.FillRectangle(&rect, &self.brush);
                            }
                        }
                        Op::Text { s, font, r, c } => {
                            if r[2] <= 1.0 {
                                continue;
                            }
                            self.brush.SetColor(&D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] });
                            let wide: Vec<u16> = s.encode_utf16().collect();
                            let rect = D2D_RECT_F { left: r[0], top: r[1], right: r[0] + r[2], bottom: r[1] + r[3] };
                            self.rt.DrawText(
                                &wide,
                                self.font(*font),
                                &rect,
                                &self.brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                                DWRITE_MEASURING_MODE_NATURAL,
                            );
                        }
                    }
                }
                let _ = self.rt.EndDraw(None, None);
            }

            let size = SIZE { cx: w_px, cy: h_px };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let dst = pos.map(|(x, y)| POINT { x, y });
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                dst.as_ref().map(|p| p as *const POINT),
                Some(&size),
                Some(self.memdc),
                Some(&src),
                windows::Win32::Foundation::COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
        (w_px, h_px)
    }
}

impl Renderer {
    /// Copy of the last drawn frame (premultiplied BGRA) and its size, for `--snapshot`.
    pub fn pixels(&self) -> Option<(Vec<u32>, i32, i32)> {
        let (_, _, bits, w, h) = self.dib?;
        let px = unsafe { std::slice::from_raw_parts(bits, (w * h) as usize) };
        Some((px.to_vec(), w, h))
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe {
            if let Some((bmp, old, _, _, _)) = self.dib.take() {
                SelectObject(self.memdc, old);
                let _ = DeleteObject(bmp.into());
            }
            let _ = DeleteDC(self.memdc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_levels_step_at_the_thresholds() {
        assert_eq!(cpu_level(0.0), 0);
        assert_eq!(cpu_level(0.5), 1);
        assert_eq!(cpu_level(4.2), 2); // one flat-out core on 24 threads
        assert_eq!(cpu_level(100.0), 4);
        const MB: u64 = 1024 * 1024;
        assert_eq!(mem_level(50 * MB), 0);
        assert_eq!(mem_level(100 * MB), 1);
        assert_eq!(mem_level(3000 * MB), 3);
        assert_eq!(mem_level(9000 * MB), 4);
    }
}

// ---------------------------------------------------------------------------------------
// Background "shader"

fn hash2(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263);
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}

fn vnoise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor() as i32, y.floor() as i32);
    let (xf, yf) = (x - x.floor(), y - y.floor());
    let (u, v) = (xf * xf * (3.0 - 2.0 * xf), yf * yf * (3.0 - 2.0 * yf));
    let a = hash2(xi, yi);
    let b = hash2(xi + 1, yi);
    let c = hash2(xi, yi + 1);
    let d = hash2(xi + 1, yi + 1);
    a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v
}

fn fbm(mut x: f32, mut y: f32) -> f32 {
    let (mut s, mut amp) = (0.0, 0.5);
    for _ in 0..4 {
        s += amp * vnoise(x, y);
        // rotate + scale between octaves to avoid grid artefacts
        let (nx, ny) = (x * 1.6 + y * 1.2, -x * 1.2 + y * 1.6);
        x = nx;
        y = ny;
        amp *= 0.5;
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn shade_background(px: &mut [u32], w: i32, h: i32, scale: f32, lights: &[Light], glow: bool, opacity: f32, t: f32) {
    let w_dip = w as f32 / scale;
    let h_dip = h as f32 / scale;
    let gw = (w_dip / GRID).ceil() as usize + 2;
    let gh = (h_dip / GRID).ceil() as usize + 2;

    // Colour on a coarse grid: dark glass + smoke lit by agent glows.
    let base = [0.030f32, 0.036, 0.052];
    let mut grid = vec![[0f32; 3]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let x = gx as f32 * GRID;
            let y = gy as f32 * GRID;
            let mut c = base;
            // faint top sheen
            let sheen = 0.035 * (1.0 - (y / 140.0).min(1.0));
            for k in 0..3 {
                c[k] += sheen;
            }
            if glow {
                // domain-warped smoke, drifting slowly upward with t
                let (sx, sy) = (x * 0.012, y * 0.012 + t);
                let wx = fbm(sx * 0.7 + 3.1, sy * 0.7 - t * 0.5);
                let wy = fbm(sx * 0.7 - 1.7, sy * 0.7 + 2.3);
                let smoke = fbm(sx + 1.8 * wx, sy + 1.8 * wy);
                let smoke = ((smoke - 0.3) * 1.6).clamp(0.0, 1.0);
                let mut lit = [0f32; 3];
                for l in lights {
                    let dx = x - l.x;
                    let dy = y - l.y;
                    let f = (-(dx * dx + dy * dy) / (l.radius * l.radius)).exp() * l.strength;
                    for k in 0..3 {
                        lit[k] += l.color[k] * f;
                    }
                }
                for k in 0..3 {
                    c[k] += (lit[k] * (0.05 + 0.42 * smoke * smoke + 0.10 * smoke)).min(0.20);
                }
            }
            grid[gy * gw + gx] = c;
        }
    }

    let r = RADIUS * scale;
    let hw = w as f32 * 0.5;
    let hh = h as f32 * 0.5;
    let inv = 1.0 / (GRID * scale);
    for y in 0..h {
        let fy = y as f32 + 0.5;
        let gyf = fy * inv;
        let gy0 = (gyf as usize).min(gh - 2);
        let ty = gyf - gy0 as f32;
        let row = &mut px[(y * w) as usize..((y + 1) * w) as usize];
        for x in 0..w {
            let fx = x as f32 + 0.5;
            // rounded-rect distance (pixels, negative inside)
            let qx = (fx - hw).abs() - (hw - r);
            let qy = (fy - hh).abs() - (hh - r);
            let d = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - r;
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover <= 0.0 {
                row[x as usize] = 0;
                continue;
            }
            let gxf = fx * inv;
            let gx0 = (gxf as usize).min(gw - 2);
            let tx = gxf - gx0 as f32;
            let i = gy0 * gw + gx0;
            let (a, b, c, e) = (grid[i], grid[i + 1], grid[i + gw], grid[i + gw + 1]);
            let mut col = [0f32; 3];
            for k in 0..3 {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bot = c[k] + (e[k] - c[k]) * tx;
                col[k] = top + (bot - top) * ty;
            }
            // thin bright rim just inside the edge
            let rim = (1.0 - ((d + 0.9 * scale).abs() / (0.9 * scale))).clamp(0.0, 1.0) * 0.10;
            let alpha = (opacity + rim).min(1.0) * cover;
            let to8 = |v: f32| ((v + rim).clamp(0.0, 1.0) * alpha * 255.0 + 0.5) as u32;
            row[x as usize] = ((alpha * 255.0 + 0.5) as u32) << 24 | to8(col[0]) << 16 | to8(col[1]) << 8 | to8(col[2]);
        }
    }
}

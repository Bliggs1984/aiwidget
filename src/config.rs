//! Settings saved to %APPDATA%\aiwidget\config.json.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Windows device name of the chosen screen, e.g. `\\.\DISPLAY2`. None = pick automatically.
    pub monitor: Option<String>,
    pub corner: Corner,
    /// Set after the widget is dragged: offset in pixels from the screen's work-area top-left.
    pub custom: Option<(i32, i32)>,
    pub on_top: bool,
    /// Size multiplier on top of Windows display scaling.
    pub scale: f32,
    pub refresh_ms: u32,
    pub show_finished: bool,
    /// The lit-smoke background. Off = flat glass panel (slightly cheaper).
    pub glow: bool,
    pub opacity: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            monitor: None,
            corner: Corner::TopRight,
            custom: None,
            on_top: false,
            scale: 1.0,
            refresh_ms: 2000,
            show_finished: true,
            glow: true,
            opacity: 0.92,
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("aiwidget")
}

fn path() -> PathBuf {
    dir().join("config.json")
}

impl Config {
    pub fn load() -> Self {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path(), s);
        }
    }
}

/// Where the optional Claude Code hook writes Claude's own command descriptions.
pub fn hook_log_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("aiwidget").join("hook-log.jsonl")
}

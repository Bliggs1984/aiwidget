//! Reads descriptions written by the optional Claude Code hook (`aiwidget-hook.exe`).
//! Only the last 64 KB of the log is read, and only when the file changes.

use std::io::{Read, Seek, SeekFrom};
use std::time::SystemTime;

use crate::agents::HookDescriptions;
use crate::config;

#[derive(Default)]
pub struct HookLog {
    last_mtime: Option<SystemTime>,
    pub map: HookDescriptions,
}

impl HookLog {
    pub fn refresh(&mut self) {
        let path = config::hook_log_path();
        let Ok(meta) = std::fs::metadata(&path) else { return };
        let mtime = meta.modified().ok();
        if mtime.is_some() && mtime == self.last_mtime {
            return;
        }
        self.last_mtime = mtime;
        let Ok(mut f) = std::fs::File::open(&path) else { return };
        let len = meta.len();
        let start = len.saturating_sub(64 * 1024);
        if f.seek(SeekFrom::Start(start)).is_err() {
            return;
        }
        let mut s = String::new();
        if f.read_to_string(&mut s).is_err() {
            return;
        }
        self.map.clear();
        for line in s.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let (Some(cmd), Some(desc)) = (v["command"].as_str(), v["description"].as_str()) else { continue };
            if !cmd.trim().is_empty() && !desc.trim().is_empty() {
                self.map.insert(cmd.trim().to_string(), desc.trim().to_string());
            }
        }
    }
}

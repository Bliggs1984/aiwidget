//! Optional Claude Code PreToolUse hook. Claude gives every Bash/PowerShell call a short
//! description; this records it so the widget can show Claude's own words instead of a guess.
//! Reads the hook JSON on stdin, appends one line to %LOCALAPPDATA%\aiwidget\hook-log.jsonl,
//! prints nothing and always exits 0 so it can never block Claude.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&input) else { return };
    let ti = &v["tool_input"];
    let (Some(cmd), Some(desc)) = (ti["command"].as_str(), ti["description"].as_str()) else { return };
    if desc.trim().is_empty() {
        return;
    }
    let line = serde_json::json!({
        "command": cmd,
        "description": desc,
        "tool": v["tool_name"],
        "cwd": v["cwd"],
        "session": v["session_id"],
    });

    let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("aiwidget");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("hook-log.jsonl");

    // Keep the log small: past 256 KB, keep only the newest half.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            if let Ok(s) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = s.lines().collect();
                let keep = lines[lines.len() / 2..].join("\n");
                let _ = std::fs::write(&path, keep + "\n");
            }
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

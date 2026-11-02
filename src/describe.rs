//! Turns a raw process command line into a short plain-English description.
//!
//! Step 1 (`effective_command`): peel off the shell wrappers agents use (Claude's
//! `bash -c "... eval '<cmd>'"`, `cmd /c`, `pwsh -Command`, base64 `-EncodedCommand`)
//! so we are left with the command the agent actually asked for.
//! Step 2 (`describe`): match that command against a table of known tools.

use crate::redact::redact;
use base64::Engine;

/// Lower-cased file name of a program token, without quotes, path or `.exe`.
pub fn prog_name(token: &str) -> String {
    let t = token.trim_matches(|c| c == '"' || c == '\'');
    let base = t.rsplit(['\\', '/']).next().unwrap_or(t);
    let lower = base.to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(s) = lower.strip_suffix(ext) {
            return s.to_string();
        }
    }
    lower
}

/// Split a command line into tokens, respecting double and single quotes.
pub fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Text after the first occurrence of any of `flags` (case-insensitive, whole token).
fn after_flag<'a>(cmd: &'a str, flags: &[&str]) -> Option<&'a str> {
    let lower = cmd.to_ascii_lowercase();
    for f in flags {
        let mut start = 0;
        while let Some(i) = lower[start..].find(f) {
            let at = start + i;
            let end = at + f.len();
            let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
            let after_ok = end == lower.len() || lower.as_bytes()[end].is_ascii_whitespace();
            if before_ok && after_ok {
                return Some(cmd[end..].trim_start());
            }
            start = end;
        }
    }
    None
}

/// If `s` starts with a quoted argument, just that argument (things after it belong to the
/// outer shell); otherwise all of `s`. Handles \" inside the quotes.
fn quoted_arg(s: &str) -> String {
    let t = s.trim_start();
    if !t.starts_with('"') {
        return t.trim().to_string();
    }
    let b = t.as_bytes();
    let mut i = 1;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 2;
            continue;
        }
        if b[i] == b'"' {
            return t[1..i].to_string();
        }
        i += 1;
    }
    t[1..].to_string()
}

fn strip_outer_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Far longer than anything we show, and it bounds the work: a command line can be 32 KB and
/// is looked at every refresh.
const MAX_COMMAND_BYTES: usize = 8 * 1024;
/// How many times `describe` will look inside a shell it finds in the command.
const MAX_DESCRIBE_DEPTH: usize = 3;

fn cut(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The command the agent actually asked to run, with wrappers removed.
pub fn effective_command(cmdline: &str) -> String {
    let mut cur = cut(cmdline, MAX_COMMAND_BYTES).trim().to_string();
    for _ in 0..4 {
        let next = unwrap_once(&cur);
        match next {
            Some(n) if n != cur && !n.is_empty() => cur = n,
            _ => break,
        }
    }
    // Replace a leading full exe path with just its name, for readability.
    let toks = tokenize(&cur);
    if let Some(first) = toks.first() {
        // (not for `NAME=C:/path` assignments)
        if (first.contains('\\') || first.contains('/')) && !first.contains('=') && first.to_ascii_lowercase().ends_with(".exe") {
            let name = prog_name(first);
            let rest_at = find_token_end(&cur);
            return format!("{}{}", name, &cur[rest_at..]).trim().to_string();
        }
    }
    cur
}

fn find_token_end(s: &str) -> usize {
    let s_trim_start = s.len() - s.trim_start().len();
    let b = s.as_bytes();
    let mut i = s_trim_start;
    if i < b.len() && b[i] == b'"' {
        i += 1;
        while i < b.len() && b[i] != b'"' {
            i += 1;
        }
        return (i + 1).min(b.len());
    }
    while i < b.len() && !b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn unwrap_once(cmd: &str) -> Option<String> {
    let toks = tokenize(cmd);
    let prog = prog_name(toks.first()?);
    match prog.as_str() {
        "bash" | "sh" | "zsh" | "dash" => {
            // Claude Code: bash -c "source snapshot ... && eval '<cmd>' < /dev/null && pwd -P >| ..."
            if let Some(i) = cmd.find("eval '") {
                let body = &cmd[i + 6..];
                let end = body
                    .rfind("' < /dev/null")
                    .or_else(|| body.rfind("' && pwd"))
                    .or_else(|| body.rfind('\''))
                    .unwrap_or(body.len());
                // Two quoting layers: the outer "..." of bash -c (\" \\ \$ \`), then the
                // single-quoted eval string ('"'"' and '\'' both mean ').
                let mut outer = String::with_capacity(end);
                let mut chars = body[..end].chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\\' && matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) {
                        outer.push(chars.next().unwrap());
                    } else {
                        outer.push(c);
                    }
                }
                return Some(outer.replace("'\"'\"'", "'").replace("'\\''", "'"));
            }
            let rest = after_flag(cmd, &["-c", "-lc", "-ic"])?;
            Some(quoted_arg(rest))
        }
        "cmd" => {
            let rest = after_flag(cmd, &["/c", "/k"])?;
            Some(strip_outer_quotes(rest).replace('^', ""))
        }
        "powershell" | "pwsh" => {
            if let Some(b64) = after_flag(cmd, &["-encodedcommand", "-enc", "-ec", "-e"]) {
                let b64 = b64.split_whitespace().next().unwrap_or("");
                let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
                let u16s: Vec<u16> = bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                return Some(String::from_utf16_lossy(&u16s));
            }
            let rest = after_flag(cmd, &["-command", "-c"])?;
            Some(quoted_arg(rest).replace("\\\"", "\""))
        }
        _ => None,
    }
}

/// Split on shell separators outside quotes: && || ; |
fn segments(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let chars: Vec<char> = cmd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                cur.push(c);
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cur.push(c);
                } else if c == ';' || c == '|' || c == '\n' || (c == '&' && chars.get(i + 1) == Some(&'&')) {
                    if c == '&' || (c == '|' && chars.get(i + 1) == Some(&'|')) {
                        i += 1;
                    }
                    out.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
        }
        i += 1;
    }
    out.push(cur);
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

const SETUP_PROGS: &[&str] = &[
    "cd", "export", "set", "source", ".", "echo", "true", "set-location", "pushd", "popd",
    "mkdir", "write-host", "write-output", "shopt", "unset", "$erroractionpreference",
];

fn first_real_segment(cmd: &str) -> String {
    let segs = segments(cmd);
    for s in &segs {
        let toks = tokenize(s);
        let Some(first) = toks.first() else { continue };
        // (a `\$` left over from an outer shell's escaping means `$`)
        let first = first.trim_start_matches('\\');
        // `$p = Get-Process x` (PowerShell): the command is the right-hand side.
        if first.starts_with('$') && !first.to_ascii_lowercase().starts_with("$env:") {
            if let Some(eq) = s.find('=') {
                let rhs = s[eq + 1..].trim();
                if !rhs.is_empty() && !rhs.starts_with(['$', '"', '\'', '@', '(', '[']) && !rhs.chars().next().unwrap().is_ascii_digit() {
                    return rhs.to_string();
                }
            }
            continue;
        }
        // `NAME=value` assignment (bash).
        if let Some(eq) = first.find('=') {
            if first[..eq].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
        }
        let p = prog_name(first);
        if p.starts_with("$env:") || SETUP_PROGS.contains(&p.as_str()) {
            continue;
        }
        return s.clone();
    }
    segs.into_iter().next().unwrap_or_default()
}

/// The command minus leading setup steps (`cd ...`, `export ...`), for display.
pub fn without_setup(cmd: &str) -> String {
    let segs = segments(cmd);
    let first = first_real_segment(cmd);
    match segs.iter().position(|s| *s == first) {
        Some(i) if i > 0 => segs[i..].join(" && "),
        _ => cmd.to_string(),
    }
}

fn first_arg(args: &[String]) -> Option<&str> {
    args.iter().map(|s| s.as_str()).find(|a| !a.starts_with('-'))
}

fn file_base(s: &str) -> String {
    s.rsplit(['\\', '/']).next().unwrap_or(s).to_string()
}

fn clip(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Name of an MCP server if this command line looks like one.
pub fn mcp_name(cmd: &str) -> Option<String> {
    // Only the program and its first two non-flag arguments count, so a long script that
    // merely mentions "mcp" somewhere isn't mistaken for a server.
    let seg = first_real_segment(cmd);
    let head: Vec<String> = tokenize(&seg)
        .into_iter()
        .enumerate()
        .filter(|(i, t)| *i == 0 || !t.starts_with('-'))
        .map(|(_, t)| t)
        .take(3)
        .collect();
    if !head.iter().any(|t| t.to_ascii_lowercase().contains("mcp")) {
        return None;
    }
    for tok in head {
        let t = tok.to_ascii_lowercase();
        if !t.contains("mcp") {
            continue;
        }
        let base = file_base(&t);
        let base = base.trim_end_matches(".exe").trim_end_matches(".js").trim_end_matches(".py");
        let base = base.split("@latest").next().unwrap_or(base);
        // @scope/mcp -> scope
        if let Some(scope) = t.strip_prefix('@') {
            let mut parts = scope.split('/');
            let s = parts.next().unwrap_or("");
            let pkg = parts.next().unwrap_or("").split('@').next().unwrap_or("");
            let pkg_clean = pkg.replace("mcp-server-", "").replace("-mcp", "").replace("mcp-", "");
            let name = if pkg == "mcp" || pkg_clean.is_empty() { s.to_string() } else { pkg_clean };
            return Some(name);
        }
        let cleaned = base
            .replace("mcp-server-", "")
            .replace("-mcp-server", "")
            .replace("mcp_server_", "")
            .replace("-mcp", "")
            .replace("_mcp", "")
            .replace("mcp-", "")
            .replace("mcp", "");
        if !cleaned.is_empty() && cleaned != "-" {
            return Some(cleaned.trim_matches(['-', '_']).to_string());
        }
    }
    Some("server".into())
}

/// Short plain-English description of an (already unwrapped) command.
pub fn describe(cmd: &str) -> String {
    describe_at(cmd, 0)
}

fn describe_at(cmd: &str, depth: usize) -> String {
    let seg = first_real_segment(cmd);
    let toks = tokenize(&seg);
    let Some(first) = toks.first() else { return "Shell command".into() };
    let prog = prog_name(first);
    let args = &toks[1..];
    let sub = first_arg(args).unwrap_or("").to_ascii_lowercase();
    let arg_line = args.join(" ");

    if let Some(name) = mcp_name(&seg) {
        return format!("MCP server: {name}");
    }
    // A shell inside the command (e.g. `powershell -Command "..."`): describe what it runs.
    if depth < MAX_DESCRIBE_DEPTH && matches!(prog.as_str(), "powershell" | "pwsh" | "cmd" | "bash" | "sh") {
        // This can decode text the caller never saw, so it has to be masked here.
        let inner = redact(&effective_command(&seg));
        if inner != seg && !inner.is_empty() {
            return describe_at(&inner, depth + 1);
        }
    }

    let d: Option<String> = match prog.as_str() {
        "git" => {
            // skip `-C path`
            let mut it = args.iter().peekable();
            let mut s = String::new();
            while let Some(a) = it.next() {
                if a == "-C" || a == "-c" {
                    it.next();
                    continue;
                }
                if !a.starts_with('-') {
                    s = a.to_ascii_lowercase();
                    break;
                }
            }
            Some(match s.as_str() {
                "status" => "Checking git status".into(),
                "diff" => "Comparing changes (git diff)".into(),
                "log" | "show" => "Reading git history".into(),
                "commit" => "Committing changes".into(),
                "push" => "Pushing to remote".into(),
                "pull" => "Pulling from remote".into(),
                "fetch" => "Fetching from remote".into(),
                "clone" => "Cloning a repository".into(),
                "checkout" | "switch" => "Switching branch".into(),
                "add" => "Staging files".into(),
                "worktree" => "Managing git worktrees".into(),
                "rebase" | "merge" => "Merging branches".into(),
                "" => "Git".into(),
                other => format!("Git {other}"),
            })
        }
        "npm" | "pnpm" | "yarn" | "bun" => {
            let script = if sub == "run" || sub == "run-script" {
                args.iter().filter(|a| !a.starts_with('-')).nth(1).map(|s| s.to_ascii_lowercase()).unwrap_or_default()
            } else {
                sub.clone()
            };
            Some(match script.as_str() {
                "install" | "i" | "add" | "ci" => "Installing packages".into(),
                "dev" | "start" | "serve" | "preview" => format!("Dev server ({prog} {script})"),
                "test" | "t" => "Running tests".into(),
                "build" => "Building project".into(),
                "lint" => "Linting code".into(),
                "typecheck" | "tsc" => "Type-checking".into(),
                "x" | "dlx" => format!("Running {}", args.get(1).map(|s| s.as_str()).unwrap_or("package")),
                "" => format!("{prog}"),
                other => format!("Running script “{other}”"),
            })
        }
        "npx" | "bunx" | "pnpx" => Some(match sub.as_str() {
            "" => "Running npx".into(),
            s if s.starts_with("vite") => "Vite dev server".into(),
            s if s.starts_with("tsc") => "Type-checking TypeScript".into(),
            s if s.starts_with("playwright") => "Running Playwright".into(),
            s => format!("Running {}", clip(s, 30)),
        }),
        "cargo" => Some(match sub.as_str() {
            "build" | "b" => "Compiling Rust".into(),
            "run" | "r" => "Running Rust program".into(),
            "test" | "t" => "Running Rust tests".into(),
            "check" | "c" => "Checking Rust code".into(),
            "clippy" => "Linting Rust code".into(),
            "add" | "remove" => "Changing Rust dependencies".into(),
            "fetch" | "update" => "Downloading crates".into(),
            "install" => "Installing a Rust tool".into(),
            "fmt" => "Formatting Rust code".into(),
            other => format!("Cargo {other}"),
        }),
        "rustc" => Some("Compiling Rust".into()),
        "rustup" | "rustup-init" => Some("Updating Rust toolchain".into()),
        "py" | "python" | "python3" | "pythonw" => Some(python_desc(args)),
        "uv" | "uvx" => Some(match sub.as_str() {
            "pip" | "add" | "sync" => "Installing Python packages".into(),
            "run" => python_desc(&args[1..]),
            s => format!("Running {}", clip(s, 30)),
        }),
        "pip" | "pip3" => Some("Installing Python packages".into()),
        "pytest" => Some("Running Python tests".into()),
        "jest" | "vitest" | "mocha" => Some("Running tests".into()),
        "node" | "deno" | "tsx" | "ts-node" => {
            if args.iter().any(|a| a == "-e" || a == "--eval" || a == "-p") {
                Some("Running a Node snippet".into())
            } else if let Some(script) = first_arg(args) {
                let s = script.to_ascii_lowercase();
                if s.contains("vite") {
                    Some("Vite dev server".into())
                } else if s.contains("next") {
                    Some("Next.js dev server".into())
                } else if s.contains("tsc") {
                    Some("Type-checking TypeScript".into())
                } else {
                    Some(format!("Node script: {}", file_base(script)))
                }
            } else {
                Some("Node".into())
            }
        }
        "vite" => Some(if sub == "build" { "Building with Vite".into() } else { "Vite dev server".into() }),
        "next" => Some(format!("Next.js {}", if sub.is_empty() { "dev".into() } else { sub.clone() })),
        "tsc" => Some("Type-checking TypeScript".into()),
        "eslint" | "biome" | "ruff" => Some("Linting code".into()),
        "prettier" | "black" => Some("Formatting code".into()),
        "rg" | "grep" | "findstr" | "select-string" | "ag" | "sls" => {
            let pat = args.iter().find(|a| !a.starts_with('-')).map(|s| clip(s, 28));
            Some(match pat {
                Some(p) => format!("Searching files for “{p}”"),
                None => "Searching files".into(),
            })
        }
        "find" | "fd" | "ls" | "dir" | "get-childitem" | "gci" | "tree" => Some("Listing files".into()),
        "cat" | "echo" | "printf" | "set-content" | "out-file" | "add-content" if seg.contains('>') || prog.contains("content") || prog == "out-file" => {
            Some("Writing a file".into())
        }
        "cat" | "head" | "tail" | "less" | "type" | "get-content" | "gc" | "wc" | "awk" => Some("Reading files".into()),
        "sed" => Some(if arg_line.contains("-i") { "Editing a file".into() } else { "Reading files".into() }),
        "curl" | "wget" | "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" => {
            let host = args.iter().find(|a| a.starts_with("http")).and_then(|u| u.split('/').nth(2)).map(|h| h.to_string());
            Some(match host {
                Some(h) => format!("Downloading from {h}"),
                None => "Downloading".into(),
            })
        }
        "ffmpeg" | "ffprobe" => Some("Processing media (ffmpeg)".into()),
        "docker" | "podman" => Some(if arg_line.contains("compose") && arg_line.contains("up") {
            "Starting containers".into()
        } else {
            format!("Docker {sub}")
        }),
        "gh" => Some(format!("GitHub: {}", clip(&args.iter().take(2).cloned().collect::<Vec<_>>().join(" "), 30))),
        "make" | "cmake" | "msbuild" | "ninja" => Some(format!("Building ({prog})")),
        "go" => Some(format!("Go {sub}")),
        "dotnet" => Some(format!(".NET {sub}")),
        "sleep" | "start-sleep" | "timeout" => Some("Waiting".into()),
        "winget" | "choco" | "scoop" => Some("Installing software".into()),
        "tar" | "unzip" | "7z" | "expand-archive" | "zip" | "compress-archive" => Some("Packing or unpacking files".into()),
        "get-ciminstance" | "get-process" | "tasklist" | "wmic" | "ps" => Some("Inspecting running processes".into()),
        "code" | "explorer" | "start" | "start-process" => Some(format!("Opening {}", clip(first_arg(args).unwrap_or("an app"), 30))),
        "magick" | "convert" => Some("Processing images".into()),
        "playwright" => Some("Running Playwright".into()),
        _ => None,
    };
    d.unwrap_or_else(|| clip(&seg, 48))
}

fn python_desc(args: &[String]) -> String {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-m" => {
                let m = it.next().map(|s| s.as_str()).unwrap_or("");
                return match m {
                    "pytest" => "Running Python tests".into(),
                    "pip" => "Installing Python packages".into(),
                    "http.server" => "Local web server".into(),
                    "venv" => "Creating a Python environment".into(),
                    other => format!("Python module {other}"),
                };
            }
            "-c" => return "Running a Python snippet".into(),
            "-" => return "Running a Python snippet".into(),
            s if s.starts_with('-') => continue,
            s => return format!("Python script: {}", file_base(s)),
        }
    }
    "Python".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_claude_bash() {
        let c = r#""C:\Program Files\Git\bin\bash.exe" -c "source /c/Users/b/.claude/shell-snapshots/snapshot-bash-1.sh 2>/dev/null || true && export TEMP='C:\x' && eval 'cargo build --release' < /dev/null && pwd -P >| /c/tmp/claude-cwd""#;
        assert_eq!(effective_command(c), "cargo build --release");
        assert_eq!(describe(&effective_command(c)), "Compiling Rust");
    }

    #[test]
    fn unwraps_quotes_in_claude_bash() {
        // As it really appears: the eval string sits inside bash -c "...", so its " are \".
        let c = r#"bash -c "eval 'echo hi; rg -n '\"'\"'foo bar'\"'\"' src' < /dev/null && pwd -P""#;
        assert_eq!(effective_command(c), "echo hi; rg -n 'foo bar' src");
        assert_eq!(describe(&effective_command(c)), "Searching files for “foo bar”");
    }

    #[test]
    fn unwraps_cmd_and_detects_mcp() {
        let c = r#"C:\WINDOWS\system32\cmd.exe /d /s /c "npx ^"@playwright/mcp@latest^"""#;
        let e = effective_command(c);
        assert_eq!(e, r#"npx "@playwright/mcp@latest""#);
        assert_eq!(mcp_name(&e).as_deref(), Some("playwright"));
        assert_eq!(describe(&e), "MCP server: playwright");
        assert_eq!(mcp_name(r#""C:\Program Files\Blackmagic Design\DaVinci Resolve\ResolveMCP.exe""#).as_deref(), Some("resolve"));
    }

    #[test]
    fn unwraps_powershell_encoded() {
        let cmd = "npm run dev";
        let utf16: Vec<u8> = cmd.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(utf16);
        let c = format!("pwsh.exe -NoProfile -EncodedCommand {b64}");
        assert_eq!(effective_command(&c), "npm run dev");
        assert_eq!(describe("npm run dev"), "Dev server (npm dev)");
    }

    #[test]
    fn skips_cd_prefix() {
        assert_eq!(describe("cd /c/x && git -C foo status"), "Checking git status");
        assert_eq!(describe("py -m pytest -q"), "Running Python tests");
        assert_eq!(describe("python tools/build.py --fast"), "Python script: build.py");
    }

    #[test]
    fn nested_shell_and_setup() {
        assert_eq!(describe(r#"export PATH="$HOME/x:$PATH" && powershell -NoProfile -Command "Stop-Process -Name foo""#), "Stop-Process -Name foo");
        assert_eq!(describe(r#"powershell -NoProfile -Command "Get-Process aiwidget""#), "Inspecting running processes");
        assert_eq!(without_setup("cd /c/x && export A=1 && cargo build"), "cargo build");
    }

    #[test]
    fn script_mentioning_mcp_is_not_a_server() {
        assert_eq!(mcp_name("py - <<'EOF'
print('mcp_name')
EOF"), None);
        assert_eq!(mcp_name("node C:/x/@modelcontextprotocol/server-mcp/index.js").is_some(), true);
    }

    #[test]
    fn assignments_and_trailing_commands() {
        let e = effective_command("T='C:/x/Temp'; ./target/release/aiwidget.exe --snapshot out.bmp");
        assert!(e.starts_with("T='C:/x/Temp';"), "{e}");
        assert_eq!(describe(&e), "./target/release/aiwidget.exe --snapshot out.bmp");
        assert_eq!(effective_command(r"C:\Program Files\nodejs\node.exe server.js"), r"C:\Program Files\nodejs\node.exe server.js");
        assert_eq!(effective_command(r"C:\nodejs\node.exe server.js"), "node server.js");
        let c = r#"powershell -NoProfile -Command "\$p=Get-Process aiwidget; Start-Sleep 60"; tail -4 log.txt"#;
        assert_eq!(describe(c), "Inspecting running processes");
        assert_eq!(describe("cat > /c/x/a.ps1 <<'EOF'"), "Writing a file");
        assert_eq!(describe("cat README.md"), "Reading files");
    }

    /// The longest command line Windows allows, as nested as it can get. Before the limits this
    /// took over a second, on the thread that runs the widget, at every refresh. The time limit
    /// is loose so a busy machine doesn't fail it.
    #[test]
    fn deeply_nested_shells_stay_cheap() {
        let cmd = format!("{}echo hi", "bash -c ".repeat(4090));
        let started = std::time::Instant::now();
        let eff = effective_command(&cmd);
        assert!(eff.len() <= MAX_COMMAND_BYTES);
        assert!(!describe(&eff).is_empty());
        assert!(started.elapsed().as_millis() < 300, "took {:?}", started.elapsed());
    }

    #[test]
    fn a_few_nested_shells_are_still_unwrapped() {
        assert_eq!(describe(&effective_command("bash -c bash -c bash -c bash -c bash -c cargo build")), "Compiling Rust");
    }

    #[test]
    fn long_commands_are_cut_safely_inside_a_multibyte_character() {
        let cmd = "é".repeat(MAX_COMMAND_BYTES);
        assert!(effective_command(&cmd).len() <= MAX_COMMAND_BYTES);
    }

    #[test]
    fn fallback_is_clipped_command() {
        assert_eq!(describe("weirdtool --flag"), "weirdtool --flag");
    }
}

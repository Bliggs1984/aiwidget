//! Works out which processes belong to which AI agent.
//!
//! An agent "session" is a running agent process (claude.exe, codex.exe, a node process
//! running the Gemini CLI, ...). Everything it starts underneath is attributed to it.
//! Each direct child is one "task" (a shell command, an MCP server, a dev server), with the
//! CPU and memory of its whole subtree added up.

use std::collections::{HashMap, HashSet};

use crate::describe;
use crate::procs::{self, EnvInfo, Proc};
use crate::redact::redact;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
    CodexSandbox,
    Gemini,
    OpenCode,
    Copilot,
    Aider,
    Cursor,
    Amp,
    Goose,
    Qwen,
    Droid,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::CodexSandbox => "Codex sandbox",
            AgentKind::Gemini => "Gemini CLI",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Copilot => "Copilot CLI",
            AgentKind::Aider => "Aider",
            AgentKind::Cursor => "Cursor Agent",
            AgentKind::Amp => "Amp",
            AgentKind::Goose => "Goose",
            AgentKind::Qwen => "Qwen Code",
            AgentKind::Droid => "Droid",
        }
    }

    /// Linear-ish RGB 0..1 used for the dot, bars and the glow behind the session.
    pub fn color(self) -> [f32; 3] {
        match self {
            AgentKind::Claude => [0.89, 0.50, 0.36],
            AgentKind::Codex | AgentKind::CodexSandbox => [0.42, 0.78, 0.98],
            AgentKind::Gemini => [0.56, 0.55, 1.00],
            AgentKind::OpenCode => [0.96, 0.80, 0.36],
            AgentKind::Copilot => [0.76, 0.56, 1.00],
            AgentKind::Aider => [0.55, 0.90, 0.46],
            AgentKind::Cursor => [0.86, 0.86, 0.90],
            AgentKind::Amp => [1.00, 0.46, 0.56],
            AgentKind::Goose => [0.96, 0.64, 0.26],
            AgentKind::Qwen => [0.42, 0.62, 1.00],
            AgentKind::Droid => [0.92, 0.60, 0.90],
        }
    }
}

/// Processes whose command line we must read to decide if they are an agent.
pub fn needs_cmd(name_lc: &str) -> bool {
    matches!(
        name_lc,
        "node.exe" | "bun.exe" | "deno.exe" | "python.exe" | "python3.exe" | "pythonw.exe" | "py.exe"
            | "claude.exe" | "codex.exe"
    )
}

pub fn classify(name_lc: &str, cmd: Option<&str>) -> Option<AgentKind> {
    let cmd_lc = cmd.map(|c| c.to_ascii_lowercase()).unwrap_or_default();
    match name_lc {
        "claude.exe" => {
            // The Claude desktop app is also claude.exe; it is an Electron app (child
            // processes carry --type=) installed under AnthropicClaude / WindowsApps.
            if cmd_lc.contains("--type=")
                || (cmd_lc.contains("anthropicclaude") && !cmd_lc.contains("claude-code"))
                || (cmd_lc.contains("\\windowsapps\\") && !cmd_lc.contains("claude-code"))
            {
                None
            } else {
                Some(AgentKind::Claude)
            }
        }
        "codex.exe" => {
            // exec-server is a helper that relays commands for Codex cloud environments.
            if cmd_lc.contains("--type=") || cmd_lc.contains(" exec-server") {
                None
            } else {
                Some(AgentKind::Codex)
            }
        }
        "codex-windows-sandbox-service.exe" => Some(AgentKind::CodexSandbox),
        "opencode.exe" => Some(AgentKind::OpenCode),
        "gemini.exe" => Some(AgentKind::Gemini),
        "copilot.exe" => Some(AgentKind::Copilot),
        "aider.exe" => Some(AgentKind::Aider),
        "cursor-agent.exe" => Some(AgentKind::Cursor),
        "amp.exe" => Some(AgentKind::Amp),
        "goose.exe" => Some(AgentKind::Goose),
        "qwen.exe" => Some(AgentKind::Qwen),
        "droid.exe" => Some(AgentKind::Droid),
        "node.exe" | "bun.exe" | "deno.exe" => {
            let c = cmd_lc.replace('\\', "/");
            if c.contains("@anthropic-ai/claude-code") || c.contains("claude-code/cli") {
                Some(AgentKind::Claude)
            } else if c.contains("@google/gemini-cli") || c.contains("/gemini-cli/") {
                Some(AgentKind::Gemini)
            } else if c.contains("@github/copilot") {
                Some(AgentKind::Copilot)
            } else if c.contains("@openai/codex") {
                Some(AgentKind::Codex)
            } else if c.contains("opencode-ai") || c.contains("/opencode/bin") {
                Some(AgentKind::OpenCode)
            } else if c.contains("@sourcegraph/amp") {
                Some(AgentKind::Amp)
            } else if c.contains("@qwen-code") {
                Some(AgentKind::Qwen)
            } else {
                None
            }
        }
        "python.exe" | "python3.exe" | "pythonw.exe" | "py.exe" => {
            let c = cmd_lc.replace('\\', "/");
            if c.contains(" -m aider") || c.contains("/aider ") || c.ends_with("/aider") || c.contains("aider-chat") {
                Some(AgentKind::Aider)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// App that hosts an agent (shown as "in T3 Code").
fn host_label(name_lc: &str, cmd_lc: Option<&str>) -> Option<&'static str> {
    if name_lc.starts_with("t3 code") || name_lc == "t3code.exe" {
        return Some("T3 Code");
    }
    match name_lc {
        "chatgpt.exe" => Some(if cmd_lc.is_some_and(|c| c.contains("openai.codex")) { "Codex app" } else { "ChatGPT app" }),
        "code.exe" => Some("VS Code"),
        "code - insiders.exe" => Some("VS Code Insiders"),
        "cursor.exe" => Some("Cursor"),
        "windsurf.exe" => Some("Windsurf"),
        "windowsterminal.exe" | "openconsole.exe" => Some("Terminal"),
        "wezterm-gui.exe" => Some("WezTerm"),
        "alacritty.exe" => Some("Alacritty"),
        "zed.exe" => Some("Zed"),
        "idea64.exe" | "pycharm64.exe" | "rustrover64.exe" | "webstorm64.exe" => Some("JetBrains IDE"),
        "claude.exe" => Some("Claude app"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub pid: u32,
    pub create_time: i64,
    pub title: String,
    pub detail: String,
    pub cpu: f32,
    pub mem: u64,
    pub io: f64,
    pub age_s: u64,
    /// Every process in this task's tree, as (pid, start time). Console hosts are left out:
    /// they aren't the work.
    pub members: Vec<(u32, i64)>,
}

impl Task {
    /// True once none of the task's processes are running. A job that outlives the command
    /// that started it means the work isn't done.
    pub fn is_finished(&self, alive: &HashSet<(u32, i64)>) -> bool {
        !self.members.iter().any(|m| alive.contains(m))
    }
}

#[derive(Clone, Debug)]
pub struct Mcp {
    pub name: String,
    pub mem: u64,
    pub cpu: f32,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub kind: AgentKind,
    pub root_pid: u32,
    pub create_time: i64,
    pub project: Option<String>,
    pub host: Option<&'static str>,
    pub via: Option<&'static str>,
    pub cpu: f32,
    pub mem: u64,
    pub io: f64,
    pub age_s: u64,
    pub tasks: Vec<Task>,
    pub mcps: Vec<Mcp>,
}

#[derive(Default)]
struct Cached {
    cmd: Option<Option<String>>,
    cwd: Option<Option<String>>,
    env: Retried<EnvInfo>,
    console: Retried<u32>,
    /// The session this process was last counted under: agent (pid, start time).
    owner: Option<(u32, i64)>,
}

/// A read that may not be ready the moment a process appears (or may not apply to it yet),
/// so a miss is retried on the next few ticks and then accepted. Processes we aren't allowed
/// to read stay misses for good, so this can't be allowed to repeat every tick.
#[derive(Default)]
struct Retried<T> {
    value: Option<T>,
    misses: u8,
}

impl<T: Copy> Retried<T> {
    const MAX_MISSES: u8 = 3;

    fn get(&mut self, read: impl FnOnce() -> Option<T>) -> Option<T> {
        if self.value.is_none() && self.misses < Self::MAX_MISSES {
            self.value = read();
            if self.value.is_none() {
                self.misses += 1;
            }
        }
        self.value
    }
}

/// Per-process cache of command lines and working folders, keyed by (pid, start time)
/// so a reused pid never inherits a dead process's details.
#[derive(Default)]
pub struct InfoCache {
    map: HashMap<(u32, i64), Cached>,
}

impl InfoCache {
    fn cmd(&mut self, p: &Proc) -> Option<&str> {
        let e = self.map.entry((p.pid, p.create_time)).or_default();
        if e.cmd.is_none() {
            e.cmd = Some(procs::read_cmdline(p.pid));
        }
        e.cmd.as_ref().unwrap().as_deref()
    }
    fn cwd(&mut self, p: &Proc) -> Option<&str> {
        let e = self.map.entry((p.pid, p.create_time)).or_default();
        if e.cwd.is_none() {
            e.cwd = Some(procs::read_cwd(p.pid));
        }
        e.cwd.as_ref().unwrap().as_deref()
    }
    fn env(&mut self, p: &Proc) -> EnvInfo {
        let e = self.map.entry((p.pid, p.create_time)).or_default();
        e.env.get(|| procs::read_env_info(p.pid)).unwrap_or_default()
    }
    fn console(&mut self, p: &Proc) -> Option<u32> {
        let e = self.map.entry((p.pid, p.create_time)).or_default();
        e.console.get(|| procs::read_console_host(p.pid))
    }
    fn owner(&self, p: &Proc) -> Option<(u32, i64)> {
        self.map.get(&(p.pid, p.create_time)).and_then(|e| e.owner)
    }
    fn set_owner(&mut self, p: &Proc, owner: (u32, i64)) {
        self.map.entry((p.pid, p.create_time)).or_default().owner = Some(owner);
    }
    /// Drop entries for processes that no longer exist. Returns the ones that do.
    pub fn retain_live(&mut self, live: &[Proc]) -> HashSet<(u32, i64)> {
        let keys: HashSet<(u32, i64)> = live.iter().map(|p| (p.pid, p.create_time)).collect();
        self.map.retain(|k, _| keys.contains(k));
        keys
    }
}

/// Descriptions reported by agents themselves (Claude Code hook), keyed by command text.
pub type HookDescriptions = HashMap<String, String>;

const SKIP_NAMES: &[&str] = &["conhost.exe", "openconsole.exe"];
const SHELLS: &[&str] = &["bash.exe", "sh.exe", "cmd.exe", "pwsh.exe", "powershell.exe", "zsh.exe"];
const MAX_TASKS: usize = 40;

/// Reconnects processes whose parent has already exited.
///
/// Git Bash loses the Windows parent link: when a script or background job goes through a
/// fork and exec, the process in between exits, so its children point at a dead pid and
/// their CPU and memory would go uncounted. Two things survive that:
///
/// - The console. The process that opened a console is the Windows parent of its conhost.exe,
///   and everything it starts stays attached, so this finds the exact command.
/// - The session it was last counted under, remembered per process. This keeps a job counted
///   after the process in between exits, for every kind of agent.
/// - `CLAUDE_PID` in the environment, which names the exact agent, for a job first seen after
///   the command that started it had gone. It is Claude Code only. A remembered session wins,
///   because a job under Codex under Claude inherits Claude's value.
fn adopt_orphans(
    procs_list: &[Proc],
    names: &[String],
    kind: &[Option<AgentKind>],
    parent: &mut [Option<usize>],
    children: &mut [Vec<usize>],
    idx: &HashMap<u32, usize>,
    cache: &mut InfoCache,
    self_pid: u32,
) {
    // Oldest first, linking as we go, so that a process's ancestry is settled before anything
    // it started is checked against it.
    let mut orphans: Vec<usize> = (0..procs_list.len())
        .filter(|&i| {
            parent[i].is_none()
                && kind[i].is_none()
                && procs_list[i].pid != self_pid
                && names[i] != "aiwidget.exe"
                && !SKIP_NAMES.contains(&names[i].as_str())
                && host_label(&names[i], None).is_none()
        })
        .collect();
    orphans.sort_by_key(|&i| procs_list[i].create_time);

    for i in orphans {
        let p = &procs_list[i];
        // Every link must point strictly back in time: that rules out a reused pid and means
        // the tree can't form a loop.
        let older = |j: usize| procs_list[j].create_time < p.create_time;

        let opener = cache
            .console(p)
            .and_then(|host| idx.get(&host).copied())
            .filter(|&h| SKIP_NAMES.contains(&names[h].as_str()))
            .and_then(|h| parent[h])
            .filter(|&o| o != i && older(o));
        let remembered = cache
            .owner(p)
            .and_then(|(pid, started)| idx.get(&pid).copied().filter(|&a| procs_list[a].create_time == started))
            .filter(|&a| kind[a].is_some());
        let agent = remembered
            .or_else(|| {
                named_agent(i, procs_list, kind, children, idx, cache).filter(|&a| kind[a] == Some(AgentKind::Claude))
            })
            .filter(|&a| older(a));

        let new_parent = match (opener, agent) {
            (Some(o), Some(a)) => Some(if is_within(o, a, parent) { o } else { a }),
            (o, a) => o.or(a),
        };
        if let Some(np) = new_parent {
            parent[i] = Some(np);
            children[np].push(i);
        }
    }
}

/// The process named by `CLAUDE_PID` for an orphan: its own value or, for a Git Bash program,
/// that of a program underneath it (see `EnvInfo::git_bash`). Doesn't look inside nested agents.
fn named_agent(
    orphan: usize,
    procs_list: &[Proc],
    kind: &[Option<AgentKind>],
    children: &[Vec<usize>],
    idx: &HashMap<u32, usize>,
    cache: &mut InfoCache,
) -> Option<usize> {
    const MAX_LOOKS: usize = 64;
    let own = cache.env(&procs_list[orphan]);
    let mut pid = own.agent_pid;
    if pid.is_none() && own.git_bash {
        let mut stack = children[orphan].clone();
        let mut looks = 0;
        while let Some(n) = stack.pop() {
            looks += 1;
            if looks > MAX_LOOKS {
                break;
            }
            if kind[n].is_some() {
                continue;
            }
            pid = cache.env(&procs_list[n]).agent_pid;
            if pid.is_some() {
                break;
            }
            stack.extend_from_slice(&children[n]);
        }
    }
    idx.get(&pid?).copied()
}

/// Is `n` the process `ancestor` or somewhere underneath it?
fn is_within(mut n: usize, ancestor: usize, parent: &[Option<usize>]) -> bool {
    for _ in 0..64 {
        if n == ancestor {
            return true;
        }
        match parent[n] {
            Some(up) => n = up,
            None => return false,
        }
    }
    false
}

pub fn build_sessions(procs_list: &[Proc], cache: &mut InfoCache, hooks: &HookDescriptions, self_pid: u32) -> Vec<Session> {
    let now = procs::now_filetime();
    let idx: HashMap<u32, usize> = procs_list.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
    let names: Vec<String> = procs_list.iter().map(|p| p.name.to_ascii_lowercase()).collect();

    // Parent links, ignoring stale ones (a parent pid reused by a younger process).
    let mut parent: Vec<Option<usize>> = procs_list
        .iter()
        .map(|p| {
            let &pi = idx.get(&p.ppid)?;
            if pi == idx[&p.pid] || procs_list[pi].create_time > p.create_time {
                None
            } else {
                Some(pi)
            }
        })
        .collect();

    // Which processes are agents.
    let mut kind: Vec<Option<AgentKind>> = vec![None; procs_list.len()];
    for (i, p) in procs_list.iter().enumerate() {
        let n = &names[i];
        let cmd = if needs_cmd(n) { cache.cmd(p).map(|s| s.to_string()) } else { None };
        kind[i] = classify(n, cmd.as_deref());
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); procs_list.len()];
    for (i, par) in parent.iter().enumerate() {
        if let Some(pi) = par {
            children[*pi].push(i);
        }
    }
    adopt_orphans(procs_list, &names, &kind, &mut parent, &mut children, &idx, cache, self_pid);

    // An agent directly under an agent of the same kind is part of it (e.g. a node launcher
    // that starts the native binary). Otherwise each agent is its own session.
    let internal: Vec<bool> = (0..procs_list.len())
        .map(|i| match (kind[i], parent[i]) {
            (Some(k), Some(pi)) => kind[pi] == Some(k),
            _ => false,
        })
        .collect();
    let is_root = |i: usize| kind[i].is_some() && !internal[i];

    let mut sessions = Vec::new();
    for root in 0..procs_list.len() {
        if !is_root(root) || procs_list[root].pid == self_pid {
            continue;
        }
        let k = kind[root].unwrap();
        let rp = &procs_list[root];

        // Gather task heads, flattening internal same-kind agent processes.
        let mut heads = Vec::new();
        let mut own_cpu = rp.cpu;
        let mut own_mem = rp.mem;
        let mut own_io = rp.io;
        let mut stack = vec![root];
        while let Some(n) = stack.pop() {
            for &c in &children[n] {
                if SKIP_NAMES.contains(&names[c].as_str()) || procs_list[c].pid == self_pid {
                    continue;
                }
                if internal[c] {
                    own_cpu += procs_list[c].cpu;
                    own_mem += procs_list[c].mem;
                    own_io += procs_list[c].io;
                    stack.push(c);
                } else if !is_root(c) {
                    heads.push(c);
                }
            }
        }

        let mut tasks = Vec::new();
        let mut mcps = Vec::new();
        let (mut cpu, mut mem, mut io) = (own_cpu, own_mem, own_io);
        for h in heads {
            // Sum the subtree, stopping at nested agent sessions.
            let (mut tcpu, mut tmem, mut tio) = (0f32, 0u64, 0f64);
            let mut members = Vec::new();
            let mut deepest_cmd_holder = h;
            let mut st = vec![(h, 0usize)];
            let mut max_depth = 0;
            while let Some((n, d)) = st.pop() {
                let p = &procs_list[n];
                if p.pid == self_pid {
                    continue;
                }
                cache.set_owner(p, (rp.pid, rp.create_time));
                if !SKIP_NAMES.contains(&names[n].as_str()) {
                    members.push((p.pid, p.create_time));
                }
                tcpu += p.cpu;
                tmem += p.mem;
                tio += p.io;
                if d > max_depth && !SHELLS.contains(&names[n].as_str()) && !SKIP_NAMES.contains(&names[n].as_str()) {
                    max_depth = d;
                    deepest_cmd_holder = n;
                }
                for &c in &children[n] {
                    if !is_root(c) {
                        st.push((c, d + 1));
                    }
                }
            }
            cpu += tcpu;
            mem += tmem;
            io += tio;

            let hp = &procs_list[h];
            let raw = cache.cmd(hp).map(|s| s.to_string());
            let mut eff = raw.as_deref().map(describe::effective_command).unwrap_or_default();
            if eff.trim().is_empty() || (SHELLS.contains(&names[h].as_str()) && describe::tokenize(&eff).len() <= 1) {
                // Couldn't read or unwrap the head; fall back to the deepest real program.
                let dp = &procs_list[deepest_cmd_holder];
                eff = cache
                    .cmd(dp)
                    .map(describe::effective_command)
                    .unwrap_or_else(|| dp.name.trim_end_matches(".exe").to_string());
            }
            if eff.trim().is_empty() {
                eff = hp.name.trim_end_matches(".exe").to_string();
            }

            // Everything that can be displayed comes from this masked copy. The hook's own
            // descriptions are keyed by the original text, so only that lookup uses `eff`.
            let shown = redact(&eff);
            if let Some(name) = describe::mcp_name(&shown) {
                mcps.push(Mcp { name, mem: tmem, cpu: tcpu });
                continue;
            }

            let title = hooks
                .get(eff.trim())
                .or_else(|| hooks.iter().find(|(c, _)| eff.contains(c.as_str()) && c.len() > 8).map(|(_, d)| d))
                .map(|d| redact(d))
                .unwrap_or_else(|| redact(&describe::describe(&shown)));
            tasks.push(Task {
                pid: hp.pid,
                create_time: hp.create_time,
                title,
                detail: describe::without_setup(&shown).split_whitespace().collect::<Vec<_>>().join(" "),
                cpu: tcpu,
                mem: tmem,
                io: tio,
                age_s: age_secs(now, hp.create_time),
                members,
            });
        }
        // Newest first.
        tasks.sort_by(|a, b| b.create_time.cmp(&a.create_time));
        tasks.truncate(MAX_TASKS);
        mcps.sort_by(|a, b| a.name.cmp(&b.name));

        // The Codex sandbox service is always running; only show it while it runs something.
        if k == AgentKind::CodexSandbox && tasks.is_empty() {
            continue;
        }

        let project = cache
            .cwd(rp)
            .map(|c| c.trim_end_matches(['\\', '/']).to_string())
            .and_then(|c| {
                let base = c.rsplit(['\\', '/']).next().unwrap_or("").to_string();
                let lower = c.to_ascii_lowercase();
                // A home, system or install folder isn't a project.
                if base.is_empty()
                    || lower.ends_with("\\system32")
                    || lower.matches('\\').count() <= 2
                    || lower.contains("\\appdata\\")
                    || lower.contains("\\program files")
                    || lower.contains("\\windowsapps\\")
                {
                    None
                } else {
                    Some(tidy_project(&base))
                }
            });

        // Walk up for a host app and a parent agent.
        let mut host = None;
        let mut via = None;
        let mut cur = parent[root];
        let mut hops = 0;
        while let Some(a) = cur {
            if via.is_none() {
                if let Some(pk) = kind[a] {
                    via = Some(pk.label());
                }
            }
            if host.is_none() {
                let n = &names[a];
                let cmd_lc = if n == "chatgpt.exe" { cache.cmd(&procs_list[a]).map(|c| c.to_ascii_lowercase()) } else { None };
                if n == "claude.exe" && kind[a].is_some() {
                    // that's a Claude Code agent, not the desktop app
                } else {
                    host = host_label(n, cmd_lc.as_deref());
                }
            }
            hops += 1;
            if hops > 32 {
                break;
            }
            cur = parent[a];
        }

        sessions.push(Session {
            kind: k,
            root_pid: rp.pid,
            create_time: rp.create_time,
            project,
            host,
            via,
            cpu,
            mem,
            io,
            age_s: age_secs(now, rp.create_time),
            tasks,
            mcps,
        });
    }
    sessions.sort_by(|a, b| a.create_time.cmp(&b.create_time));
    sessions
}

/// T3 Code scratch folders look like `2026-11-02-fix-the-thing-f684b4d7`; show `fix the thing`.
fn tidy_project(base: &str) -> String {
    let b = base.as_bytes();
    let dated = b.len() > 20 && b[4] == b'-' && b[7] == b'-' && b[10] == b'-' && base[..4].bytes().all(|c| c.is_ascii_digit());
    if dated {
        let mut rest = &base[11..];
        if let Some(i) = rest.rfind('-') {
            if rest.len() - i == 9 && rest[i + 1..].bytes().all(|c| c.is_ascii_hexdigit()) {
                rest = &rest[..i];
            }
        }
        return rest.replace('-', " ");
    }
    base.to_string()
}

fn age_secs(now: i64, created: i64) -> u64 {
    ((now - created).max(0) / 10_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_agents() {
        assert_eq!(classify("claude.exe", Some(r"C:\Users\b\.local\bin\claude.EXE --output-format stream-json")), Some(AgentKind::Claude));
        assert_eq!(classify("claude.exe", Some(r#""C:\Users\b\AppData\Local\AnthropicClaude\app-1.0\claude.exe" --type=renderer"#)), None);
        assert_eq!(classify("codex.exe", Some("codex.exe app-server")), Some(AgentKind::Codex));
        assert_eq!(classify("node.exe", Some(r"node C:\npm\node_modules\@google\gemini-cli\dist\index.js")), Some(AgentKind::Gemini));
        assert_eq!(classify("node.exe", Some("node server.js")), None);
        assert_eq!(classify("python.exe", Some("python -m aider --model x")), Some(AgentKind::Aider));
    }

    #[test]
    fn tidies_scratch_names() {
        assert_eq!(tidy_project("2026-11-02-set-t3-codes-default-starting-f684b4d7"), "set t3 codes default starting");
        assert_eq!(tidy_project("aiwidget"), "aiwidget");
    }

    fn p(pid: u32, ppid: u32, name: &str, t: i64, cpu: f32) -> Proc {
        Proc { pid, ppid, name: name.into(), create_time: t, cpu, mem: 1_000_000, io: 0.0 }
    }

    #[test]
    fn stale_parent_pid_is_ignored() {
        // pid 10 is a *younger* process than its "child" 11, so 11 is not its child.
        let list = vec![p(10, 1, "codex.exe", 500, 0.0), p(11, 10, "cargo.exe", 100, 5.0)];
        let mut cache = InfoCache::default();
        // codex needs cmd; reading a fake pid fails and returns None, which still classifies.
        let s = build_sessions(&list, &mut cache, &HashMap::new(), 0);
        assert_eq!(s.len(), 1);
        assert!(s[0].tasks.is_empty());
    }

    const CLAUDE: u32 = 100;

    /// What the cache should report for one process.
    #[derive(Default, Clone, Copy)]
    struct Seen {
        pid: u32,
        agent_pid: Option<u32>,
        git_bash: bool,
        console: Option<u32>,
    }

    /// Fills the cache so these tests never read real processes.
    fn sessions(list: &[Proc], seen: &[Seen]) -> Vec<Session> {
        let mut cache = InfoCache::default();
        seed(&mut cache, list, seen);
        build_sessions(list, &mut cache, &HashMap::new(), 0)
    }

    fn seed(cache: &mut InfoCache, list: &[Proc], seen: &[Seen]) {
        for p in list {
            let cmd = if p.name == "claude.exe" { "claude.exe --print".to_string() } else { format!("{} job", p.name) };
            let s = seen.iter().find(|s| s.pid == p.pid).copied().unwrap_or_default();
            cache.map.insert(
                (p.pid, p.create_time),
                Cached {
                    cmd: Some(Some(cmd)),
                    cwd: Some(None),
                    env: Retried { value: Some(EnvInfo { agent_pid: s.agent_pid, git_bash: s.git_bash }), misses: 0 },
                    console: Retried { value: s.console, misses: Retried::<u32>::MAX_MISSES },
                    owner: None,
                },
            );
        }
    }

    #[test]
    fn a_counted_job_stays_counted_after_the_process_in_between_exits() {
        // Tick 1: the job is under a shell, under Codex. No environment clue at all.
        let codex = p(CLAUDE, 1, "codex.exe", 100, 0.0);
        let shell = p(200, CLAUDE, "powershell.exe", 200, 0.0);
        let job = p(400, 200, "python.exe", 400, 25.0);
        let mut cache = InfoCache::default();
        let first = vec![codex.clone(), shell, job.clone()];
        seed(&mut cache, &first, &[]);
        assert_eq!(build_sessions(&first, &mut cache, &HashMap::new(), 0)[0].cpu, 25.0);
        // Tick 2: the shell has exited.
        let second = vec![codex, job];
        let s = build_sessions(&second, &mut cache, &HashMap::new(), 0);
        assert_eq!(task(&s[0], 400).cpu, 25.0);
    }

    #[test]
    fn a_job_stays_with_the_inner_agent_despite_the_outer_agents_marker() {
        // Claude starts Codex; Codex starts a job, which inherits Claude's CLAUDE_PID.
        let claude = p(50, 1, "claude.exe", 50, 0.0);
        let codex = p(CLAUDE, 50, "codex.exe", 100, 0.0);
        let shell = p(200, CLAUDE, "powershell.exe", 200, 0.0);
        let job = p(400, 200, "python.exe", 400, 25.0);
        let named = [Seen { pid: 400, agent_pid: Some(50), ..Default::default() }];
        let mut cache = InfoCache::default();
        let first = vec![claude.clone(), codex.clone(), shell, job.clone()];
        seed(&mut cache, &first, &named);
        build_sessions(&first, &mut cache, &HashMap::new(), 0);
        let second = vec![claude, codex, job];
        let s = build_sessions(&second, &mut cache, &HashMap::new(), 0);
        let inner = s.iter().find(|x| x.root_pid == CLAUDE).unwrap();
        let outer = s.iter().find(|x| x.root_pid == 50).unwrap();
        assert_eq!(inner.cpu, 25.0);
        assert_eq!(outer.cpu, 0.0);
    }

    #[test]
    fn a_remembered_owner_with_a_reused_pid_is_ignored() {
        let job = p(400, 200, "python.exe", 400, 25.0);
        let first = vec![p(CLAUDE, 1, "codex.exe", 100, 0.0), p(200, CLAUDE, "cmd.exe", 200, 0.0), job.clone()];
        let mut cache = InfoCache::default();
        seed(&mut cache, &first, &[]);
        build_sessions(&first, &mut cache, &HashMap::new(), 0);
        // A new Codex now has the old one's pid, with a different start time.
        let second = vec![p(CLAUDE, 1, "codex.exe", 150, 0.0), job];
        let s = build_sessions(&second, &mut cache, &HashMap::new(), 0);
        assert!(s[0].tasks.is_empty());
    }

    #[test]
    fn a_worker_stays_with_the_inner_agent_when_its_opener_is_reconnected_the_same_tick() {
        // Claude starts Codex, and a shell under Codex opens a console.
        let claude = p(50, 1, "claude.exe", 50, 0.0);
        let codex = p(CLAUDE, 50, "codex.exe", 100, 0.0);
        let shell = p(200, CLAUDE, "powershell.exe", 200, 0.0);
        let opener = p(300, 200, "bash.exe", 300, 0.0);
        let conhost = p(350, 300, "conhost.exe", 350, 0.0);
        let mut cache = InfoCache::default();
        let first = vec![claude.clone(), codex.clone(), shell, opener.clone(), conhost.clone()];
        seed(&mut cache, &first, &[]);
        build_sessions(&first, &mut cache, &HashMap::new(), 0);
        // Next tick the shell has gone, and a worker has appeared with a dead parent, sharing
        // that console and carrying the outer agent's marker. List it first, so only sorting
        // by age can settle the opener before the worker.
        let worker = p(400, 999, "python.exe", 400, 25.0);
        let second = vec![worker, claude, codex, opener, conhost];
        seed(&mut cache, &second[..1], &[Seen { pid: 400, agent_pid: Some(50), console: Some(350), ..Default::default() }]);
        let s = build_sessions(&second, &mut cache, &HashMap::new(), 0);
        let inner = s.iter().find(|x| x.root_pid == CLAUDE).unwrap();
        let outer = s.iter().find(|x| x.root_pid == 50).unwrap();
        assert_eq!(inner.cpu, 25.0);
        assert_eq!(outer.cpu, 0.0);
    }

    #[test]
    fn a_task_is_finished_only_when_none_of_its_processes_are_left() {
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(200, CLAUDE, "bash.exe", 200, 0.0),
            p(250, 200, "conhost.exe", 250, 0.0),
            p(400, 200, "python.exe", 400, 25.0),
        ];
        let t = sessions(&list, &[])[0].tasks[0].clone();
        assert_eq!(t.pid, 200);
        // The shell has gone but the job it started is running: not done.
        assert!(!t.is_finished(&HashSet::from([(400, 400)])));
        // Only a lingering console host is left: done.
        assert!(t.is_finished(&HashSet::from([(250, 250)])));
        // Same pid, reused by a new process: done.
        assert!(t.is_finished(&HashSet::from([(400, 999)])));
    }

    #[test]
    fn failed_reads_are_retried_a_few_times_then_left_alone() {
        let mut r: Retried<u32> = Retried::default();
        let mut reads = 0;
        for _ in 0..10 {
            assert_eq!(r.get(|| { reads += 1; None }), None);
        }
        assert_eq!(reads, Retried::<u32>::MAX_MISSES);
        // A read that succeeds on a retry is kept and not repeated.
        let mut r: Retried<u32> = Retried::default();
        assert_eq!(r.get(|| None), None);
        assert_eq!(r.get(|| Some(7)), Some(7));
        assert_eq!(r.get(|| panic!("already known")), Some(7));
    }

    fn task(s: &Session, pid: u32) -> &Task {
        s.tasks.iter().find(|t| t.pid == pid).unwrap()
    }

    /// The real shape: Git Bash's script process has a dead parent and no CLAUDE_PID, but shares
    /// a console opened by a live command. Another command started later must not claim it.
    #[test]
    fn orphan_follows_its_console_to_the_exact_command() {
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(200, CLAUDE, "bash.exe", 200, 0.0),
            p(250, 200, "conhost.exe", 250, 0.0),
            p(300, CLAUDE, "bash.exe", 300, 0.0),
            p(400, 999, "bash.exe", 400, 0.0),
            p(500, 400, "python.exe", 500, 25.0),
        ];
        let s = sessions(&list, &[Seen { pid: 400, console: Some(250), git_bash: true, ..Default::default() }]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].tasks.len(), 2);
        assert_eq!(s[0].cpu, 25.0);
        assert_eq!(task(&s[0], 200).cpu, 25.0);
        assert_eq!(task(&s[0], 300).cpu, 0.0);
    }

    #[test]
    fn job_that_outlived_its_command_is_found_through_the_program_it_runs() {
        // No console left to follow. The Git Bash stub has no CLAUDE_PID, but its child does.
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(400, 999, "bash.exe", 400, 0.0),
            p(500, 400, "node.exe", 500, 25.0),
        ];
        let s = sessions(
            &list,
            &[
                Seen { pid: 400, git_bash: true, ..Default::default() },
                Seen { pid: 500, agent_pid: Some(CLAUDE), git_bash: true, ..Default::default() },
            ],
        );
        assert_eq!(s[0].tasks.len(), 1);
        assert_eq!(task(&s[0], 400).cpu, 25.0);
    }

    #[test]
    fn only_git_bash_orphans_borrow_a_childs_value() {
        // explorer.exe has a dead parent too. An app under it must not pull it into a session.
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(400, 999, "explorer.exe", 400, 25.0),
            p(500, 400, "notepad.exe", 500, 25.0),
        ];
        let s = sessions(&list, &[Seen { pid: 500, agent_pid: Some(CLAUDE), ..Default::default() }]);
        assert!(s[0].tasks.is_empty());
    }

    #[test]
    fn a_nested_agents_children_do_not_speak_for_the_orphan_above_it() {
        // A plain Git Bash starts Claude. The tools under that Claude must not attach the shell
        // to any agent, even when one of them names an older Claude.
        let list = vec![
            p(50, 1, "claude.exe", 50, 0.0),
            p(400, 999, "bash.exe", 400, 0.0),
            p(CLAUDE, 400, "claude.exe", 500, 0.0),
            p(600, CLAUDE, "cmd.exe", 600, 5.0),
        ];
        let s = sessions(
            &list,
            &[
                Seen { pid: 400, git_bash: true, ..Default::default() },
                Seen { pid: 600, agent_pid: Some(50), ..Default::default() },
            ],
        );
        assert_eq!(s.len(), 2);
        assert!(s[0].tasks.is_empty());
        assert_eq!(s[1].tasks.len(), 1);
        assert_eq!(s[1].via, None);
    }

    #[test]
    fn orphan_that_opened_its_own_console_falls_back_to_the_agent_named() {
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(400, 999, "bash.exe", 400, 25.0),
            p(450, 400, "conhost.exe", 450, 0.0),
        ];
        let s = sessions(&list, &[Seen { pid: 400, agent_pid: Some(CLAUDE), console: Some(450), ..Default::default() }]);
        assert_eq!(task(&s[0], 400).cpu, 25.0);
    }

    #[test]
    fn the_agent_named_wins_when_the_console_leads_outside_it() {
        // Claude running in a terminal: the console opener is the terminal, not Claude.
        let list = vec![
            p(10, 1, "WindowsTerminal.exe", 10, 0.0),
            p(20, 10, "OpenConsole.exe", 20, 0.0),
            p(CLAUDE, 10, "claude.exe", 100, 0.0),
            p(400, 999, "python.exe", 400, 25.0),
        ];
        let s = sessions(&list, &[Seen { pid: 400, agent_pid: Some(CLAUDE), console: Some(20), ..Default::default() }]);
        assert_eq!(task(&s[0], 400).cpu, 25.0);
    }

    #[test]
    fn console_id_that_is_not_a_console_host_is_ignored() {
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(200, CLAUDE, "bash.exe", 200, 0.0),
            p(400, 999, "python.exe", 400, 25.0),
        ];
        let s = sessions(&list, &[Seen { pid: 400, console: Some(200), ..Default::default() }]);
        assert_eq!(s[0].cpu, 0.0);
    }

    #[test]
    fn orphan_is_left_alone_when_the_named_process_is_not_its_agent() {
        // Pid reused: the "agent" started after the orphan.
        let younger = vec![p(CLAUDE, 1, "claude.exe", 900, 0.0), p(400, 999, "python.exe", 400, 25.0)];
        let named = [Seen { pid: 400, agent_pid: Some(CLAUDE), ..Default::default() }];
        assert!(sessions(&younger, &named)[0].tasks.is_empty());
        // Pid reused by something that is not an agent.
        let other = vec![
            p(50, 1, "claude.exe", 50, 0.0),
            p(CLAUDE, 1, "notepad.exe", 100, 0.0),
            p(400, 999, "python.exe", 400, 25.0),
        ];
        let s = sessions(&other, &named);
        assert_eq!(s.len(), 1);
        assert!(s[0].tasks.is_empty());
        // Nothing to go on.
        let none = vec![p(CLAUDE, 1, "claude.exe", 100, 0.0), p(400, 999, "python.exe", 400, 25.0)];
        assert!(sessions(&none, &[])[0].tasks.is_empty());
    }

    #[test]
    fn secrets_do_not_reach_the_panel() {
        let list = vec![p(CLAUDE, 1, "claude.exe", 100, 0.0), p(200, CLAUDE, "bash.exe", 200, 0.0), p(300, CLAUDE, "bash.exe", 300, 0.0)];
        let mut cache = InfoCache::default();
        seed(&mut cache, &list, &[]);
        let curl = r#"bash -c "curl -H 'Authorization: Bearer SECRET-ONE' https://me:SECRET-TWO@x.dev?token=SECRET-THREE""#;
        cache.map.get_mut(&(200, 200)).unwrap().cmd = Some(Some(curl.to_string()));
        cache.map.get_mut(&(300, 300)).unwrap().cmd = Some(Some("npx -y @x/mcp-server --api-key SECRET-FOUR".to_string()));

        let s = &build_sessions(&list, &mut cache, &HashMap::new(), 0)[0];
        let shown = format!("{:?} {:?}", s.tasks, s.mcps);
        assert!(!shown.contains("SECRET"), "{shown}");
        assert_eq!(s.tasks.len(), 1, "{shown}");
        assert_eq!(s.mcps.len(), 1, "{shown}");

        // A description from the hook is still matched, and is masked too.
        let cmd = r#"curl -H 'Authorization: Bearer SECRET-ONE' https://me:SECRET-TWO@x.dev?token=SECRET-THREE"#;
        let hooks = HashMap::from([(cmd.to_string(), "Calling the API with token=SECRET-FIVE".to_string())]);
        let s = &build_sessions(&list, &mut cache, &hooks, 0)[0];
        assert_eq!(s.tasks[0].title, "Calling the API with token=•••");
    }

    /// A nested encoded PowerShell command is only decoded while writing the title, after the
    /// command text was masked, so the title has to be masked as well.
    #[test]
    fn secrets_decoded_while_describing_do_not_reach_the_title() {
        use base64::Engine;
        let inner: Vec<u8> = "tool --password SECRET-ONE".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD.encode(inner);
        let list = vec![p(CLAUDE, 1, "claude.exe", 100, 0.0), p(200, CLAUDE, "bash.exe", 200, 0.0)];
        let mut cache = InfoCache::default();
        seed(&mut cache, &list, &[]);
        let cmd = format!("cd /x && powershell -EncodedCommand {b64}");
        cache.map.get_mut(&(200, 200)).unwrap().cmd = Some(Some(cmd));
        let s = &build_sessions(&list, &mut cache, &HashMap::new(), 0)[0];
        assert_eq!(s.tasks.len(), 1);
        assert!(!s.tasks[0].title.contains("SECRET"), "{}", s.tasks[0].title);
    }

    #[test]
    fn host_apps_and_the_widget_are_never_adopted() {
        let list = vec![
            p(CLAUDE, 1, "claude.exe", 100, 0.0),
            p(400, 999, "T3 Code.exe", 400, 25.0),
            p(500, 999, "aiwidget.exe", 500, 1.0),
            p(600, 999, "conhost.exe", 600, 1.0),
        ];
        let named = |pid| Seen { pid, agent_pid: Some(CLAUDE), ..Default::default() };
        let s = sessions(&list, &[named(400), named(500), named(600)]);
        assert!(s[0].tasks.is_empty());
        assert_eq!(s[0].cpu, 0.0);
    }
}

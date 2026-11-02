<img src="assets/icon.svg" width="64" height="64" alt="">

# AIWidget

A small see-through panel on your desktop that shows which AI agents are running, what each
one has started, and how much CPU and memory that work uses.

- **Agents it recognises:** Claude Code, Codex, Gemini CLI, OpenCode, Copilot CLI, Aider,
  Cursor Agent, Amp, Goose, Qwen Code, Droid. It also shows the app hosting each one (T3 Code,
  Codex app, VS Code, Terminal and so on) and the project folder.
- **Per task:** a short description ("Compiling Rust", "Searching files for "foo"", "Dev
  server (npm dev)"), the actual command, CPU %, memory, disk speed when busy, and how long
  it has run. MCP servers are grouped on one line. Finished tasks stay for 20 seconds as "done".
- **Icons:** a chip that fills from the bottom shows CPU, and a memory stick with four chips
  shows memory. Each has four steps, with the exact number beside it:

  | Step | CPU (% of the whole PC) | Memory |
  |---|---|---|
  | 1 | 0.5% | 100 MB |
  | 2 | 4% (about one full core here) | 500 MB |
  | 3 | 15% | 2 GB |
  | 4 | 40% | 8 GB |

- **Looks:** dark glass panel with soft smoke behind it, lit by one coloured glow per agent.
  A glow gets brighter as that agent works harder.

## How it finds an agent's work

Everything under an agent in the Windows process tree counts as its work. Git Bash breaks that
tree: scripts and background jobs often have a parent that has already exited. Those processes
are reconnected, using in order:

1. The console they share, which leads to the exact command that opened it.
2. The session they were last counted under.
3. `CLAUDE_PID`, which Claude Code sets for everything it starts. Codex has no equivalent.

A job for another agent can still be missed if its parent exits before the widget first sees it
(it looks every 2 seconds) and no console links it back. A job that keeps running after its
command has ended shows on its own row, and that command doesn't get a "done" row.

"Done" rows have two small gaps. A command can show as done if everything it had started exits
within one refresh and leaves a brand-new process running. A task that was past the row limit
when it finished never shows as done.

## Privacy

The panel shows what agents run, so values that look secret are hidden as `•••`: passwords and
keys in flags and variables (`--token`, `API_KEY=`), `Authorization` and `Cookie` headers, logins
in URLs, and well-known key formats. This works from patterns, so a secret with no telling name
or shape can still appear. Keep that in mind before sharing a screenshot.

## Performance

Speed comes first, so there is no GPU use, no animation loop and no background thread.

- Every 2 seconds it makes one Windows call that returns the whole process list (the same
  call Task Manager uses). It reads command lines once per new process and keeps them.
- It redraws only when something you can see has changed, in software, in a few milliseconds.
- It runs in Windows "Efficiency mode" (lowest priority, power-saving scheduling).
- Measured on this PC (24 logical CPUs):
  - With a few rows showing: about 22 MB of memory and 0.6% of one core.
  - With a full panel: 30 to 35 MB and about 1 to 1.7% of one core, under 0.1% of the whole
    PC. Short runs varied from 0.7% to 2.6%, and memory sometimes briefly reached 44 MB.
- It doesn't start with Windows. Run it when you want it.

## Use

Run `target\release\aiwidget.exe`.

- **Drag** it anywhere. It remembers the spot.
- **Right-click** for: Screen, Corner, Layer (behind windows or always on top), Size,
  Refresh speed, Glow on/off, Show recently finished, Close.
- Settings live in `%APPDATA%\aiwidget\config.json`.

## Optional: Claude's own descriptions

Claude Code writes a one-line description for every shell command it runs. If you turn on
this hook, the widget shows Claude's description instead of its own guess. Add the following
to `%USERPROFILE%\.claude\settings.json`:

```json
"hooks": {
  "PreToolUse": [
    {
      "matcher": "Bash|PowerShell",
      "hooks": [
        { "type": "command", "command": "C:\\Utils\\AIstuff\\aiwidget\\target\\release\\aiwidget-hook.exe" }
      ]
    }
  ]
}
```

The hook only appends a line to `%LOCALAPPDATA%\aiwidget\hook-log.jsonl` and always exits
cleanly, so it can't block Claude.

**Before turning it on:** that file stores each command in full and in plain text, including any
password or key in it. The masking above only affects what the panel shows, not this file.

## Build

Needs Rust (`rustup`) and the Visual Studio C++ Build Tools.

```
cargo build --release
cargo test --release
```

For debugging: set `AIWIDGET_DEBUG=1` to log timings to `%TEMP%\aiwidget-debug.log`.
`aiwidget.exe --snapshot out.bmp` draws one frame to an image file without opening a window.

## Code map

| File | Job |
|---|---|
| `src/procs.rs` | Reads the process list, CPU, memory, command lines, working folders |
| `src/agents.rs` | Recognises agents and groups their child processes into tasks |
| `src/describe.rs` | Unwraps shell wrappers and turns commands into plain English |
| `src/redact.rs` | Hides secret-looking values in commands before they are shown |
| `src/render.rs` | Lays out the panel, draws the smoke background and the text |
| `src/main.rs` | Window, timer, right-click menu, dragging, screen placement |
| `src/screens.rs` | Lists the screens |
| `src/hooklog.rs`, `src/bin/aiwidget-hook.rs` | Optional Claude Code descriptions |

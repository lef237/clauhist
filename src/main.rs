use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Local;
use clap::{Parser, Subcommand};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
struct HistoryEntry {
    #[serde(rename = "sessionId", default)]
    session_id: String,
    display: Option<String>,
    timestamp: Option<u64>, // milliseconds since epoch
    project: Option<String>,
}

#[derive(Debug)]
struct Session {
    session_id: String,
    project: String,
    first_ts: u64,
    last_ts: u64,
    messages: Vec<(u64, String)>, // (timestamp_ms, display)
}

#[derive(Parser)]
#[command(
    name = "clauhist",
    version,
    about = "Browse Claude Code history across working directories and resume sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[arg(long, hide = true)]
    print: bool,
    /// Exit the clauhist sub-shell and return to the original shell
    #[arg(long = "return")]
    return_flag: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Show session preview (used internally by fzf --preview)
    Preview { session_id: String },
    /// Print shell integration code for your shell
    Init {
        /// Shell name (zsh, bash, fish, nu)
        shell: String,
    },
}

/// The `CLAUDE_CONFIG_DIR` the user set themselves, if any. `None` is not the
/// same as `Some("~/.claude")`: Claude Code reads `~/.claude.json` when the
/// variable is absent but `$CLAUDE_CONFIG_DIR/.claude.json` when it is set, so
/// forwarding the fallback would point Claude at a file that does not exist and
/// start it on a fresh, logged-out profile.
fn claude_config_dir_override() -> Option<String> {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|d| !d.is_empty())
}

/// Claude Code keeps its data in `~/.claude` unless `CLAUDE_CONFIG_DIR` points
/// elsewhere. clauhist has to follow the same rule, otherwise a relocated
/// installation looks like it has no history at all.
fn claude_config_dir() -> PathBuf {
    if let Some(dir) = claude_config_dir_override() {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("Neither CLAUDE_CONFIG_DIR nor HOME is set");
        std::process::exit(1);
    });
    PathBuf::from(home).join(".claude")
}

fn history_file() -> PathBuf {
    claude_config_dir().join("history.jsonl")
}

fn parse_sessions(content: &str) -> HashMap<String, Vec<HistoryEntry>> {
    let mut sessions: HashMap<String, Vec<HistoryEntry>> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
            if !entry.session_id.is_empty() {
                sessions
                    .entry(entry.session_id.clone())
                    .or_default()
                    .push(entry);
            }
        }
    }
    sessions
}

/// Entries of a single session. fzf re-runs the preview command on every cursor
/// move, so this skips the JSON parse for lines that cannot belong to the
/// session instead of parsing the whole history the way the browser does.
fn parse_session_entries(content: &str, session_id: &str) -> Vec<HistoryEntry> {
    content
        .lines()
        .filter(|line| line.contains(session_id))
        .filter_map(|line| serde_json::from_str::<HistoryEntry>(line).ok())
        .filter(|entry| entry.session_id == session_id)
        .collect()
}

fn read_history() -> String {
    std::fs::read_to_string(history_file()).unwrap_or_default()
}

fn build_session(session_id: String, mut entries: Vec<HistoryEntry>) -> Session {
    entries.sort_by_key(|e| e.timestamp.unwrap_or(0));
    let project = entries
        .first()
        .and_then(|e| e.project.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let first_ts = entries.first().and_then(|e| e.timestamp).unwrap_or(0);
    let last_ts = entries.last().and_then(|e| e.timestamp).unwrap_or(0);
    let messages = entries
        .iter()
        .filter_map(|e| {
            let display = e.display.clone().unwrap_or_default();
            if display.is_empty() {
                None
            } else {
                Some((e.timestamp.unwrap_or(0), display))
            }
        })
        .collect();
    Session {
        session_id,
        project,
        first_ts,
        last_ts,
        messages,
    }
}

fn build_sessions(raw: HashMap<String, Vec<HistoryEntry>>) -> Vec<Session> {
    let mut sessions: Vec<Session> = raw
        .into_iter()
        .map(|(session_id, entries)| build_session(session_id, entries))
        .collect();
    sessions.sort_by(|a, b| b.last_ts.cmp(&a.last_ts));
    sessions
}

fn truncate(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > max_chars {
        let truncated: String = chars[..max_chars].iter().collect();
        format!("{}…", truncated)
    } else {
        text.to_string()
    }
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn format_ts_datetime(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn format_ts_time(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "??:??".to_string())
}

fn fzf_is_available() -> bool {
    Command::new("fzf")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn format_for_fzf(sessions: &[Session]) -> Vec<String> {
    sessions
        .iter()
        .map(|s| {
            let date_str = format_ts_datetime(s.last_ts);
            let exists = if std::path::Path::new(&s.project).exists() {
                "✓"
            } else {
                "✗"
            };
            let first_msg = s
                .messages
                .first()
                .map(|(_, msg)| truncate(&msg.replace(['\t', '\n'], " "), 70))
                .unwrap_or_default();
            format!(
                "{}\t{}\t{} {}\t{}\t({})",
                s.session_id,
                date_str,
                exists,
                s.project,
                first_msg,
                s.messages.len()
            )
        })
        .collect()
}

fn render_preview(session: &Session) -> String {
    let mut output = format!(
        "Project : {}\nSession : {}\nStarted : {}\nLast    : {}\nMessages: {}\n{}\n",
        session.project,
        session.session_id,
        format_ts_datetime(session.first_ts),
        format_ts_datetime(session.last_ts),
        session.messages.len(),
        "─".repeat(60)
    );

    for (ts, msg) in &session.messages {
        let clean = msg.replace('\n', " ");
        output.push_str(&format!(
            "[{}] {}\n",
            format_ts_time(*ts),
            truncate(&clean, 120)
        ));
    }

    output
}

fn build_resume_cmd(
    project: &str,
    session_id: &str,
    shell: &str,
    config_dir: Option<&str>,
    zdotdir: Option<&str>,
    prev_dir: Option<&str>,
    depth: u32,
) -> String {
    // `env CLAUDE_CONFIG_DIR=... claude` rather than an inline `VAR=value`
    // prefix: the generated command is run through `sh` and the same shape is
    // built by the shell wrappers, one of which (fish) has no inline
    // assignment. `env` is portable across all of them. Without an override the
    // prefix is dropped entirely — see `claude_config_dir_override`.
    let config_dir_prefix = config_dir
        .map(|d| format!("env CLAUDE_CONFIG_DIR={} ", shell_quote(d)))
        .unwrap_or_default();
    let base = format!(
        "cd {} && {}claude --resume {}",
        shell_quote(project),
        config_dir_prefix,
        shell_quote(session_id)
    );
    // Keep the sub-shell on the same account, so a plain `claude` there
    // resolves to the config directory the session was resumed from.
    let config_dir_env = config_dir
        .map(|d| format!("CLAUDE_CONFIG_DIR={} ", shell_quote(d)))
        .unwrap_or_default();
    let zdotdir_env = zdotdir
        .map(|d| format!("ZDOTDIR={} ", shell_quote(d)))
        .unwrap_or_default();
    let prev_dir_env = prev_dir
        .map(|d| format!("CLAUHIST_PREV_DIR={} ", shell_quote(d)))
        .unwrap_or_default();
    let back_msg = prev_dir
        .map(|d| format!("Type exit or clauhist --return to go back to {d}."))
        .unwrap_or_else(|| "Type exit or clauhist --return to go back.".to_string());
    let ended_msg = shell_quote(&format!("Claude session ended. {back_msg}"));
    // `$$` is the PID of the `sh` running this command, and `exec` keeps it,
    // so the interactive shell ends up recording its own PID. `clauhist
    // --return` checks it before signalling anything.
    format!(
        "{}; echo ''; echo {ended_msg}; CLAUHIST_SHELL={depth} CLAUHIST_SHELL_PID=$$ {prev_dir_env}{zdotdir_env}{config_dir_env}exec {} -i",
        base,
        shell_quote(shell)
    )
}

/// Contract used by the shell wrappers: the project path, the session id, and —
/// only when the user set `CLAUDE_CONFIG_DIR` — the config directory to resume
/// under. Two lines mean "resume without setting the variable at all", which is
/// what keeps a default installation on its usual `~/.claude.json` profile. The
/// wrappers split on newlines, so every value must be newline-free for the
/// contract to round-trip; returns `None` when any of them would break it.
fn print_contract(project: &str, session_id: &str, config_dir: Option<&str>) -> Option<String> {
    if project.contains('\n') || session_id.contains('\n') {
        return None;
    }
    match config_dir {
        Some(dir) if dir.contains('\n') => None,
        Some(dir) => Some(format!("{project}\n{session_id}\n{dir}")),
        None => Some(format!("{project}\n{session_id}")),
    }
}

/// Interactive shell to hand back to after Claude exits. The generated command
/// is run through `sh`, so any shell works as the exec target.
fn resume_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zsh".to_string())
}

fn shell_is_zsh(shell: &str) -> bool {
    std::path::Path::new(shell)
        .file_name()
        .map(|n| n == "zsh")
        .unwrap_or(false)
}

/// Replaces `path` in one step: write a private temporary file next to it, then
/// rename it into place. `std::fs::write` truncates before it writes, so a
/// reader — the zsh of a sub-shell started at the same moment, or a second
/// clauhist run at the same depth — can otherwise catch the file empty or
/// half-written. Errors are ignored, like the plain write it replaces: a
/// missing prompt indicator must not stop a session from being resumed.
fn write_atomically(path: &Path, contents: &str) {
    // A process id alone is not unique enough: `cargo test` drives this from
    // several threads inside one process.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let Some(dir) = path.parent() else { return };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clauhist");
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    if std::fs::write(&tmp, contents).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn setup_clauhist_zdotdir(depth: u32) -> PathBuf {
    let home = std::env::var("HOME").expect("HOME environment variable must be set");
    let dir = PathBuf::from(home)
        .join(".cache")
        .join("clauhist")
        .join(format!("zdotdir-{depth}"));
    let _ = std::fs::create_dir_all(&dir);

    let orig_zdotdir =
        std::env::var("ZDOTDIR").unwrap_or_else(|_| std::env::var("HOME").unwrap_or_default());

    let indicator = if depth > 1 {
        format!("[clauhist({depth})]")
    } else {
        "[clauhist]".to_string()
    };

    // zsh reads $ZDOTDIR/.zshenv before $ZDOTDIR/.zshrc, so the temporary ZDOTDIR
    // has to forward .zshenv as well — otherwise PATH and anything else set there
    // is missing from the sub-shell. ZDOTDIR is pointed back here afterwards so the
    // .zshrc below is still the one zsh picks up.
    let zshenv = format!(
        "[[ -f {orig}/.zshenv ]] && source {orig}/.zshenv\n\
         ZDOTDIR={here}\n",
        orig = shell_quote(&orig_zdotdir),
        here = shell_quote(&dir.to_string_lossy()),
    );

    let zshrc = format!(
        "ZDOTDIR={orig}\n\
         [[ -f \"$ZDOTDIR/.zshrc\" ]] && source \"$ZDOTDIR/.zshrc\"\n\
         PROMPT=\"%F{{cyan}}{indicator}%f $PROMPT\"\n",
        orig = shell_quote(&orig_zdotdir),
    );

    write_atomically(&dir.join(".zshenv"), &zshenv);
    write_atomically(&dir.join(".zshrc"), &zshrc);
    dir
}

fn clauhist_depth() -> u32 {
    std::env::var("CLAUHIST_SHELL")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
}

fn is_clauhist_shell() -> bool {
    clauhist_depth() > 0
}

fn cmd_init(shell: &str) {
    match shell {
        "zsh" | "bash" => {
            println!(
                r#"clauhist() {{
    local out
    out=$(command clauhist --print "$@") || return
    [[ "$out" == *$'\n'* ]] || return
    local project="${{out%%$'\n'*}}"
    local rest="${{out#*$'\n'}}"
    local sid="${{rest%%$'\n'*}}"
    local cfg=""
    [[ "$rest" == *$'\n'* ]] && cfg="${{rest#*$'\n'}}"
    [[ "$cfg" != *$'\n'* ]] || return
    cd -- "$project" || return
    if [[ -n "$cfg" ]]; then
        CLAUDE_CONFIG_DIR="$cfg" claude --resume "$sid"
    else
        claude --resume "$sid"
    fi
}}"#
            );
        }
        "fish" => {
            println!(
                r#"function clauhist
    set -l out (command clauhist --print $argv)
    set -l n (count $out)
    if test $n -lt 2 -o $n -gt 3
        return
    end
    cd $out[1]; or return
    if test $n -eq 3
        env CLAUDE_CONFIG_DIR=$out[3] claude --resume $out[2]
    else
        claude --resume $out[2]
    end
end"#
            );
        }
        "nu" => {
            println!(
                r#"def --env clauhist [...args: string] {{
    let result = (^clauhist --print ...$args | complete)
    if $result.exit_code != 0 {{
        print --stderr $result.stderr
        return
    }}
    let lines = ($result.stdout | str trim --right | lines)
    let n = ($lines | length)
    if $n < 2 or $n > 3 {{ return }}
    cd ($lines | get 0)
    if $n == 3 {{
        with-env {{ CLAUDE_CONFIG_DIR: ($lines | get 2) }} {{
            ^claude --resume ($lines | get 1)
        }}
    }} else {{
        ^claude --resume ($lines | get 1)
    }}
}}"#
            );
        }
        _ => {
            eprintln!(
                "Unsupported shell: {}. Supported: zsh, bash, fish, nu",
                shell
            );
            std::process::exit(1);
        }
    }
}

fn cmd_preview(session_id: &str, content: &str) {
    let entries = parse_session_entries(content, session_id);
    if entries.is_empty() {
        println!("Session not found: {}", session_id);
        return;
    }
    let session = build_session(session_id.to_string(), entries);
    print!("{}", render_preview(&session));
}

fn cmd_return() {
    if !is_clauhist_shell() {
        eprintln!("Not inside a clauhist sub-shell.");
        std::process::exit(1);
    }

    // Being someone's child is not proof of whose child: PIDs are reused, and
    // a shell started by hand inside the sub-shell inherits CLAUHIST_SHELL too.
    // The sub-shell records its own PID, so only signal a parent that matches it.
    let ppid = unsafe { libc::getppid() };
    let recorded = std::env::var("CLAUHIST_SHELL_PID")
        .ok()
        .and_then(|v| v.parse::<i32>().ok());
    if recorded != Some(ppid) {
        eprintln!("clauhist --return only works directly in the clauhist sub-shell.");
        eprintln!("Type exit to leave the current shell.");
        std::process::exit(1);
    }

    let prev_dir = std::env::var("CLAUHIST_PREV_DIR").ok();
    match &prev_dir {
        Some(d) => eprintln!("Returned to previous shell. ({d})"),
        None => eprintln!("Returned to previous shell."),
    }

    // SIGHUP, not SIGKILL: the shell runs its normal exit path, so zsh and bash
    // still write their history file — SIGKILL threw away everything typed in
    // the sub-shell. SIGTERM would be ignored by an interactive bash; SIGHUP is
    // what closing a terminal sends, and no shell prints a warning for it.
    if unsafe { libc::kill(ppid, libc::SIGHUP) } != 0 {
        eprintln!(
            "Failed to signal the clauhist sub-shell: {}",
            std::io::Error::last_os_error()
        );
        std::process::exit(1);
    }

    std::process::exit(0);
}

fn cmd_browse(sessions: Vec<Session>, print_mode: bool, exe_path: &str) {
    if !fzf_is_available() {
        eprintln!("fzf not found. Install with: brew install fzf");
        std::process::exit(1);
    }

    let lines = format_for_fzf(&sessions);
    let fzf_input = lines.join("\n");
    let preview_cmd = format!("{} preview {{1}}", shell_quote(exe_path));

    let fzf_args: Vec<String> = vec![
        "--delimiter=\t".to_string(),
        "--with-nth=2,3,4,5".to_string(),
        format!("--preview={}", preview_cmd),
        "--preview-window=down:50%:wrap".to_string(),
        "--height=85%".to_string(),
        "--border=rounded".to_string(),
        "--header=Claude Code History Browser  [Enter: resume  Ctrl-O: toggle preview  Ctrl-C: cancel]"
            .to_string(),
        "--prompt=Search: ".to_string(),
        "--no-sort".to_string(),
        // fzf treats ctrl-/ as an alias for ctrl-_ (0x1F), which some terminals
        // (e.g. WezTerm) never emit. ctrl-o is a plain ASCII control character,
        // so it works everywhere.
        "--bind=ctrl-o:toggle-preview".to_string(),
        "--bind=ctrl-/:toggle-preview".to_string(),
    ];

    let mut child = match Command::new("fzf")
        .args(&fzf_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to spawn fzf: {}", e);
            std::process::exit(1);
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(fzf_input.as_bytes());
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("fzf error: {}", e);
            std::process::exit(1);
        }
    };

    // Exit cleanly on Ctrl-C (130), no-match (1), or any cancellation
    if !output.status.success() {
        std::process::exit(0);
    }

    let line = String::from_utf8_lossy(&output.stdout);
    let line = line.trim();
    if line.is_empty() {
        std::process::exit(0);
    }

    let fields: Vec<&str> = line.splitn(5, '\t').collect();
    if fields.len() < 3 {
        eprintln!("Unexpected fzf output format");
        std::process::exit(1);
    }

    let session_id = fields[0];
    let project = fields[2].trim_start_matches(['✓', '✗', ' ']);

    // The resume command is `cd <project> && claude --resume <id>`, so a missing
    // directory means claude never starts. Say so instead of failing silently.
    if !std::path::Path::new(project).exists() {
        eprintln!("Project directory no longer exists: {}", project);
        eprintln!("Sessions marked ✗ cannot be resumed.");
        std::process::exit(1);
    }

    // Only a real override is forwarded: a default install has to stay on the
    // unset-variable code path, or Claude Code looks for its account in
    // ~/.claude/.claude.json and starts logged out.
    let config_dir = claude_config_dir_override();

    if print_mode {
        // Three-line contract for shell wrappers: project, session id, then the
        // config directory to resume under. Each shell formats its own `cd` +
        // `claude --resume` from these.
        match print_contract(project, session_id, config_dir.as_deref()) {
            Some(out) => println!("{out}"),
            None => {
                eprintln!("Cannot resume: the project path, session id, or config directory contains a newline.");
                std::process::exit(1);
            }
        }
    } else {
        let depth = clauhist_depth() + 1;
        let shell = resume_shell();
        // The prompt indicator is a zsh-only trick; other shells get a plain sub-shell.
        let zdotdir = if shell_is_zsh(&shell) {
            Some(setup_clauhist_zdotdir(depth).to_string_lossy().into_owned())
        } else {
            None
        };
        let prev_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .ok();
        let shell_cmd = build_resume_cmd(
            project,
            session_id,
            &shell,
            config_dir.as_deref(),
            zdotdir.as_deref(),
            prev_dir.as_deref(),
            depth,
        );
        if let Err(e) = Command::new("sh").arg("-c").arg(&shell_cmd).status() {
            eprintln!("Failed to start shell: {}", e);
            std::process::exit(1);
        }
    }
}

fn main() {
    let cli = Cli::parse();

    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "clauhist".to_string());

    if cli.return_flag {
        cmd_return();
        return;
    }

    match cli.command {
        Some(Commands::Init { shell }) => {
            cmd_init(&shell);
        }
        Some(Commands::Preview { session_id }) => {
            cmd_preview(&session_id, &read_history());
        }
        None => {
            let sessions = build_sessions(parse_sessions(&read_history()));
            if sessions.is_empty() {
                let path = history_file();
                if !path.exists() {
                    eprintln!("History file not found: {}", path.display());
                } else {
                    eprintln!("No history found");
                }
                std::process::exit(1);
            }
            cmd_browse(sessions, cli.print, &exe_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn history_entry(
        session_id: &str,
        display: Option<&str>,
        timestamp: Option<u64>,
        project: Option<&str>,
    ) -> HistoryEntry {
        HistoryEntry {
            session_id: session_id.to_string(),
            display: display.map(str::to_string),
            timestamp,
            project: project.map(str::to_string),
        }
    }

    fn home_path(relative: &str) -> String {
        let home = std::env::var("HOME").unwrap();
        format!("{home}/{relative}")
    }

    fn unique_temp_path(label: &str) -> PathBuf {
        // Tests run in parallel and the clock can report the same nanos twice,
        // so a counter is what actually keeps two callers apart: sharing a
        // directory means one test deletes it while the other still needs it.
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let home = std::env::var("HOME").unwrap();
        PathBuf::from(home)
            .join("tmp")
            .join(format!("clauhist-{label}-{suffix}-{seq}"))
    }

    #[test]
    fn parse_sessions_skips_blank_invalid_and_missing_session_ids() {
        let p = home_path("projects/a");
        let raw = format!(
            "\n\
             {{\"sessionId\":\"alpha\",\"display\":\"first\",\"timestamp\":10,\"project\":\"{p}\"}}\n\
             not json\n\
             {{\"sessionId\":\"\",\"display\":\"ignored\",\"timestamp\":20,\"project\":\"{p}\"}}\n\
             \n\
             {{\"sessionId\":\"alpha\",\"display\":\"second\",\"timestamp\":30,\"project\":\"{p}\"}}\n"
        );

        let sessions = parse_sessions(&raw);

        assert_eq!(sessions.len(), 1);
        let alpha = sessions.get("alpha").unwrap();
        assert_eq!(alpha.len(), 2);
        assert_eq!(alpha[0].display.as_deref(), Some("first"));
        assert_eq!(alpha[1].display.as_deref(), Some("second"));
    }

    #[test]
    fn build_sessions_sorts_entries_filters_empty_messages_and_orders_by_recent_activity() {
        let older_proj = home_path("projects/older");
        let newer_proj = home_path("projects/newer");
        let mut raw = HashMap::new();
        raw.insert(
            "older".to_string(),
            vec![
                history_entry("older", Some("later"), Some(30), Some(&older_proj)),
                history_entry("older", Some(""), Some(20), Some(&older_proj)),
                history_entry("older", Some("first"), Some(10), Some(&older_proj)),
            ],
        );
        raw.insert(
            "newer".to_string(),
            vec![history_entry(
                "newer",
                Some("recent"),
                Some(100),
                Some(&newer_proj),
            )],
        );

        let sessions = build_sessions(raw);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "newer");
        assert_eq!(sessions[0].last_ts, 100);

        let older = &sessions[1];
        assert_eq!(older.project, older_proj);
        assert_eq!(older.first_ts, 10);
        assert_eq!(older.last_ts, 30);
        assert_eq!(older.messages.len(), 2);
        assert_eq!(older.messages[0], (10, "first".to_string()));
        assert_eq!(older.messages[1], (30, "later".to_string()));
    }

    #[test]
    fn parse_session_entries_only_returns_the_requested_session() {
        let p = home_path("projects/a");
        let raw = format!(
            "\n\
             {{\"sessionId\":\"alpha\",\"display\":\"first\",\"timestamp\":10,\"project\":\"{p}\"}}\n\
             not json\n\
             {{\"sessionId\":\"beta\",\"display\":\"talks about alpha\",\"timestamp\":20,\"project\":\"{p}\"}}\n\
             {{\"sessionId\":\"alpha\",\"display\":\"second\",\"timestamp\":30,\"project\":\"{p}\"}}\n"
        );

        let entries = parse_session_entries(&raw, "alpha");

        // The beta line mentions "alpha" in its text, so the cheap line filter
        // lets it through — the session id check has to drop it.
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].display.as_deref(), Some("first"));
        assert_eq!(entries[1].display.as_deref(), Some("second"));
        assert!(parse_session_entries(&raw, "missing").is_empty());
    }

    #[test]
    fn targeted_parse_renders_the_same_preview_as_the_full_parse() {
        let p = home_path("projects/a");
        let raw = format!(
            "{{\"sessionId\":\"alpha\",\"display\":\"later\",\"timestamp\":30,\"project\":\"{p}\"}}\n\
             {{\"sessionId\":\"beta\",\"display\":\"other\",\"timestamp\":40,\"project\":\"{p}\"}}\n\
             {{\"sessionId\":\"alpha\",\"display\":\"\",\"timestamp\":20,\"project\":\"{p}\"}}\n\
             {{\"sessionId\":\"alpha\",\"display\":\"first\",\"timestamp\":10,\"project\":\"{p}\"}}\n"
        );

        let all = build_sessions(parse_sessions(&raw));
        let from_full = all.iter().find(|s| s.session_id == "alpha").unwrap();
        let targeted = build_session("alpha".to_string(), parse_session_entries(&raw, "alpha"));

        assert_eq!(render_preview(&targeted), render_preview(from_full));
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("こんにちは世界", 4), "こんにち…");
        assert_eq!(truncate("rust", 4), "rust");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        let p = home_path("projects/it's here");
        let expected = format!("'{}'", p.replace("'", "'\\''"));
        assert_eq!(shell_quote(&p), expected);
    }

    #[test]
    fn format_for_fzf_marks_existing_projects_and_sanitizes_message_preview() {
        let existing_dir = unique_temp_path("project");
        std::fs::create_dir_all(&existing_dir).unwrap();

        let session = Session {
            session_id: "session-1".to_string(),
            project: existing_dir.display().to_string(),
            first_ts: 0,
            last_ts: 0,
            messages: vec![(0, "hello\tworld\nagain".to_string())],
        };

        let lines = format_for_fzf(&[session]);
        let fields: Vec<&str> = lines[0].split('\t').collect();

        assert_eq!(fields[0], "session-1");
        assert!(fields[2].starts_with(&format!("✓ {}", existing_dir.display())));
        assert_eq!(fields[3], "hello world again");
        assert_eq!(fields[4], "(1)");

        std::fs::remove_dir_all(existing_dir).unwrap();
    }

    #[test]
    fn build_resume_cmd_includes_subshell_and_env_var() {
        let p = home_path("projects/my-project");
        let zd = home_path("projects/zd");
        let cfg = home_path(".claude");
        let home = std::env::var("HOME").unwrap();
        let cmd = build_resume_cmd(
            &p,
            "abc-123",
            "/bin/zsh",
            Some(cfg.as_str()),
            Some(&zd),
            Some(&home),
            1,
        );
        assert!(cmd.starts_with(&format!(
            "cd '{p}' && env CLAUDE_CONFIG_DIR='{cfg}' claude --resume 'abc-123';"
        )));
        assert!(cmd.contains("CLAUHIST_SHELL=1"));
        assert!(cmd.contains(&format!("CLAUDE_CONFIG_DIR='{cfg}'")));
        assert!(cmd.contains(&format!("ZDOTDIR='{zd}'")));
        assert!(cmd.contains(&format!("CLAUHIST_PREV_DIR='{home}'")));
        assert!(cmd.contains(&format!("go back to {home}")));
        assert!(cmd.contains("exec '/bin/zsh' -i"));
        assert!(cmd.contains("clauhist --return"));
    }

    #[test]
    fn build_resume_cmd_nested_depth_is_reflected() {
        let p = home_path("projects/p");
        let cmd = build_resume_cmd(
            &p,
            "s1",
            "zsh",
            Some(home_path(".claude").as_str()),
            None,
            None,
            3,
        );
        assert!(cmd.contains("CLAUHIST_SHELL=3"));
    }

    #[test]
    fn build_resume_cmd_without_zdotdir() {
        let p = home_path("projects/my-project");
        let cmd = build_resume_cmd(
            &p,
            "abc-123",
            "zsh",
            Some(home_path(".claude").as_str()),
            None,
            None,
            1,
        );
        assert!(cmd.contains("CLAUHIST_SHELL=1 CLAUHIST_SHELL_PID=$$ "));
        assert!(cmd.contains("exec 'zsh' -i"));
        assert!(!cmd.contains("ZDOTDIR"));
        assert!(cmd.contains("go back."));
    }

    #[test]
    fn build_resume_cmd_omits_the_env_var_without_an_override() {
        let p = home_path("projects/my-project");
        let cmd = build_resume_cmd(&p, "abc-123", "zsh", None, None, None, 1);
        // Passing CLAUDE_CONFIG_DIR=~/.claude is NOT the same as leaving it
        // unset: Claude Code would then look for the account in
        // ~/.claude/.claude.json and start on a fresh, logged-out profile.
        assert!(cmd.starts_with(&format!("cd '{p}' && claude --resume 'abc-123';")));
        assert!(!cmd.contains("CLAUDE_CONFIG_DIR"));
    }

    #[test]
    fn build_resume_cmd_execs_the_users_shell() {
        let p = home_path("projects/p");
        let cfg = home_path(".claude");
        for shell in ["/opt/homebrew/bin/bash", "/usr/local/bin/fish"] {
            let cmd = build_resume_cmd(&p, "s1", shell, Some(cfg.as_str()), None, None, 1);
            assert!(cmd.contains(&format!("exec '{shell}' -i")));
        }
    }

    #[test]
    fn build_resume_cmd_records_the_subshell_pid() {
        let p = home_path("projects/p");
        let cmd = build_resume_cmd(
            &p,
            "s1",
            "zsh",
            Some(home_path(".claude").as_str()),
            None,
            None,
            1,
        );
        // Deliberately unquoted: sh expands $$ to the PID that exec hands to the shell.
        assert!(cmd.contains("CLAUHIST_SHELL_PID=$$ "));
    }

    /// The PID must belong to the shell `clauhist --return` will signal — the
    /// one exec replaced sh with, not some intermediate process.
    #[test]
    fn resume_cmd_records_the_pid_of_the_exec_ed_shell() {
        use std::os::unix::fs::PermissionsExt;

        let project = unique_temp_path("project");
        std::fs::create_dir_all(&project).unwrap();
        let fake_shell = project.join("fake-shell");
        std::fs::write(&fake_shell, "#!/bin/sh\necho \"$CLAUHIST_SHELL_PID $$\"\n").unwrap();
        std::fs::set_permissions(&fake_shell, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cmd = build_resume_cmd(
            &project.to_string_lossy(),
            "s1",
            &fake_shell.to_string_lossy(),
            Some(home_path(".claude").as_str()),
            None,
            None,
            1,
        );
        // Absolute path: the empty PATH below would make "sh" itself unresolvable.
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&cmd)
            // Empty PATH keeps `env ... claude` from finding a real claude.
            .env("PATH", "")
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let last = stdout.lines().last().unwrap_or_default();
        let (recorded, actual) = last.split_once(' ').unwrap_or(("", "-"));
        assert_eq!(
            recorded, actual,
            "CLAUHIST_SHELL_PID must be the shell's own PID, got: {stdout:?}"
        );

        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn build_resume_cmd_quotes_session_id() {
        let p = home_path("projects/p");
        let cmd = build_resume_cmd(
            &p,
            "s1; rm -rf /",
            "zsh",
            Some(home_path(".claude").as_str()),
            None,
            None,
            1,
        );
        assert!(cmd.contains("claude --resume 's1; rm -rf /'"));
    }

    #[test]
    fn print_contract_is_project_session_then_config_dir() {
        assert_eq!(
            print_contract("/tmp/my-project", "abc-123", Some("/home/u/.claude")).as_deref(),
            Some("/tmp/my-project\nabc-123\n/home/u/.claude")
        );
    }

    #[test]
    fn print_contract_is_two_lines_without_an_override() {
        // No third line: the wrappers then resume without setting
        // CLAUDE_CONFIG_DIR at all.
        assert_eq!(
            print_contract("/tmp/my-project", "abc-123", None).as_deref(),
            Some("/tmp/my-project\nabc-123")
        );
    }

    #[test]
    fn print_contract_rejects_newlines() {
        // The wrappers split on newlines, so a newline in any value would
        // silently shift the project/session/config boundary.
        assert_eq!(
            print_contract("/tmp/a\nb", "abc-123", Some("/home/u/.claude")),
            None
        );
        assert_eq!(
            print_contract("/tmp/proj", "a\nb", Some("/home/u/.claude")),
            None
        );
        assert_eq!(
            print_contract("/tmp/proj", "abc-123", Some("/home\nu/.claude")),
            None
        );
    }

    #[test]
    fn build_resume_cmd_quotes_prev_dir_in_message() {
        let p = home_path("projects/p");
        let prev = home_path("it's here");
        let cmd = build_resume_cmd(
            &p,
            "s1",
            "zsh",
            Some(home_path(".claude").as_str()),
            None,
            Some(&prev),
            1,
        );
        // The message is a single echo argument, so the quote in the path must be escaped.
        assert!(cmd.contains(&format!(
            "echo 'Claude session ended. Type exit or clauhist --return to go back to {}.'",
            prev.replace('\'', "'\\''")
        )));
    }

    #[test]
    fn build_resume_cmd_quotes_project_path_and_config_dir_with_special_chars() {
        let p = home_path("projects/it's here");
        let cfg = home_path("it's/.claude-work");
        let cmd = build_resume_cmd(&p, "sess-1", "zsh", Some(cfg.as_str()), None, None, 1);
        assert!(cmd.starts_with(&format!(
            "cd {} && env CLAUDE_CONFIG_DIR={} claude --resume 'sess-1';",
            shell_quote(&p),
            shell_quote(&cfg)
        )));
    }

    #[test]
    fn shell_is_zsh_matches_on_basename_only() {
        assert!(shell_is_zsh("zsh"));
        assert!(shell_is_zsh("/bin/zsh"));
        assert!(shell_is_zsh("/opt/homebrew/bin/zsh"));
        assert!(!shell_is_zsh("/bin/bash"));
        assert!(!shell_is_zsh("/usr/local/bin/fish"));
        assert!(!shell_is_zsh("/bin/zsh-static"));
    }

    #[test]
    fn setup_clauhist_zdotdir_depth_1_shows_clauhist() {
        let dir = setup_clauhist_zdotdir(1);
        let content = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(content.contains("[clauhist]"));
        assert!(!content.contains("[clauhist("));
    }

    #[test]
    fn setup_clauhist_zdotdir_depth_2_shows_clauhist_with_number() {
        let dir = setup_clauhist_zdotdir(2);
        let content = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(content.contains("[clauhist(2)]"));
    }

    #[test]
    fn setup_clauhist_zdotdir_forwards_zshenv_then_restores_itself() {
        let dir = setup_clauhist_zdotdir(1);
        let orig = std::env::var("ZDOTDIR").unwrap_or_else(|_| std::env::var("HOME").unwrap());
        let content = std::fs::read_to_string(dir.join(".zshenv")).unwrap();

        assert!(content.contains(&format!("source {}/.zshenv", shell_quote(&orig))));
        // ZDOTDIR must point back here so zsh still reads the .zshrc we generated.
        assert!(content.contains(&format!("ZDOTDIR={}", shell_quote(&dir.to_string_lossy()))));
    }

    /// Every clauhist run at the same depth writes the same two files, so a
    /// reader must never catch one empty or half-written. Without the rename
    /// this fails within a few iterations — and it is also what made the two
    /// depth-1 tests above flaky under `cargo test`'s default parallelism.
    #[test]
    fn setup_clauhist_zdotdir_survives_concurrent_runs() {
        let readers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..25 {
                        let dir = setup_clauhist_zdotdir(1);
                        let zshrc = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
                        assert!(zshrc.contains("[clauhist]"), "torn .zshrc: {zshrc:?}");
                        let zshenv = std::fs::read_to_string(dir.join(".zshenv")).unwrap();
                        assert!(zshenv.contains("ZDOTDIR="), "torn .zshenv: {zshenv:?}");
                    }
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
    }

    /// End-to-end check of the .zshenv forwarding against a real zsh.
    #[test]
    fn generated_zdotdir_sources_zshenv_in_a_real_zsh() {
        let fake_home = unique_temp_path("home");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::fs::write(
            fake_home.join(".zshenv"),
            "export CLAUHIST_TEST_VAR=from_zshenv\n",
        )
        .unwrap();
        std::fs::write(fake_home.join(".zshrc"), "").unwrap();

        let zdotdir = unique_temp_path("zdotdir");
        std::fs::create_dir_all(&zdotdir).unwrap();
        std::fs::write(
            zdotdir.join(".zshenv"),
            format!(
                "[[ -f {orig}/.zshenv ]] && source {orig}/.zshenv\nZDOTDIR={here}\n",
                orig = shell_quote(&fake_home.to_string_lossy()),
                here = shell_quote(&zdotdir.to_string_lossy()),
            ),
        )
        .unwrap();
        std::fs::write(
            zdotdir.join(".zshrc"),
            "print -r -- \"$CLAUHIST_TEST_VAR\"\n",
        )
        .unwrap();

        let output = std::process::Command::new("zsh")
            .args(["-i", "-c", "true"])
            .env("HOME", &fake_home)
            .env("ZDOTDIR", &zdotdir)
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("from_zshenv"),
            "user's .zshenv must be sourced in the sub-shell, got: {stdout:?}"
        );

        std::fs::remove_dir_all(fake_home).unwrap();
        std::fs::remove_dir_all(zdotdir).unwrap();
    }

    #[test]
    fn render_preview_formats_metadata_and_message_lines() {
        let p = home_path("projects/example");
        let invalid_ms = i64::MAX as u64;
        let preview = render_preview(&Session {
            session_id: "session-1".to_string(),
            project: p.clone(),
            first_ts: invalid_ms,
            last_ts: invalid_ms,
            messages: vec![(invalid_ms, "line one\nline two".to_string())],
        });

        assert!(preview.contains(&format!("Project : {p}")));
        assert!(preview.contains("Session : session-1"));
        assert!(preview.contains("Started : unknown"));
        assert!(preview.contains("Last    : unknown"));
        assert!(preview.contains("Messages: 1"));
        assert!(preview.contains("[??:??] line one line two"));
    }
}

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    about = "Browse and resume Claude Code chat sessions"
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
        /// Shell name (zsh, bash, fish)
        shell: String,
    },
}

fn history_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("HOME environment variable not set");
        std::process::exit(1);
    });
    PathBuf::from(home).join(".claude").join("history.jsonl")
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

fn read_sessions() -> HashMap<String, Vec<HistoryEntry>> {
    let path = history_file();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    parse_sessions(&content)
}

fn build_sessions(raw: HashMap<String, Vec<HistoryEntry>>) -> Vec<Session> {
    let mut sessions: Vec<Session> = raw
        .into_iter()
        .map(|(session_id, mut entries)| {
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
        })
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

fn build_resume_cmd(project: &str, session_id: &str, print_mode: bool, zdotdir: Option<&str>, prev_dir: Option<&str>, depth: u32) -> String {
    let base = format!(
        "cd {} && claude --resume {}",
        shell_quote(project),
        session_id
    );
    if print_mode {
        base
    } else {
        let zdotdir_env = zdotdir
            .map(|d| format!("ZDOTDIR={} ", shell_quote(d)))
            .unwrap_or_default();
        let prev_dir_env = prev_dir
            .map(|d| format!("CLAUHIST_PREV_DIR={} ", shell_quote(d)))
            .unwrap_or_default();
        let back_msg = prev_dir
            .map(|d| format!("Type exit or clauhist --return to go back to {d}."))
            .unwrap_or_else(|| "Type exit or clauhist --return to go back.".to_string());
        format!(
            "{}; echo ''; echo 'Claude session ended. {back_msg}'; CLAUHIST_SHELL={depth} {prev_dir_env}{zdotdir_env}exec zsh -i",
            base
        )
    }
}

fn setup_clauhist_zdotdir(depth: u32) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = PathBuf::from(home).join(".cache").join("clauhist").join(format!("zdotdir-{depth}"));
    let _ = std::fs::create_dir_all(&dir);

    let orig_zdotdir = std::env::var("ZDOTDIR")
        .unwrap_or_else(|_| std::env::var("HOME").unwrap_or_default());

    let indicator = if depth > 1 {
        format!("[clauhist({depth})]")
    } else {
        "[clauhist]".to_string()
    };

    let zshrc = format!(
        "ZDOTDIR={orig}\n\
         [[ -f \"$ZDOTDIR/.zshrc\" ]] && source \"$ZDOTDIR/.zshrc\"\n\
         PROMPT=\"%F{{cyan}}{indicator}%f $PROMPT\"\n",
        orig = shell_quote(&orig_zdotdir),
    );

    let _ = std::fs::write(dir.join(".zshrc"), zshrc);
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
        "zsh" => {
            println!(
                r#"function clauhist() {{ local cmd=$(command clauhist --print "$@"); [[ -n "$cmd" ]] && eval "$cmd"; }}"#
            );
        }
        "bash" => {
            println!(
                r#"function clauhist() {{ local cmd=$(command clauhist --print "$@"); [[ -n "$cmd" ]] && eval "$cmd"; }}"#
            );
        }
        "fish" => {
            println!(
                r#"function clauhist
    set -l cmd (command clauhist --print $argv)
    if test -n "$cmd"
        eval $cmd
    end
end"#
            );
        }
        _ => {
            eprintln!("Unsupported shell: {}. Supported: zsh, bash, fish", shell);
            std::process::exit(1);
        }
    }
}

fn cmd_preview(session_id: &str, raw: HashMap<String, Vec<HistoryEntry>>) {
    let sessions = build_sessions(raw);
    let session = match sessions.iter().find(|s| s.session_id == session_id) {
        Some(s) => s,
        None => {
            println!("Session not found: {}", session_id);
            return;
        }
    };
    print!("{}", render_preview(session));
}

fn get_ppid() -> Option<i32> {
    let pid = std::process::id();
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<i32>()
        .ok()
}

fn cmd_return() {
    if !is_clauhist_shell() {
        eprintln!("Not inside a clauhist sub-shell.");
        std::process::exit(1);
    }

    let ppid = match get_ppid() {
        Some(p) => p,
        None => {
            eprintln!("Could not determine parent shell PID.");
            std::process::exit(1);
        }
    };

    let prev_dir = std::env::var("CLAUHIST_PREV_DIR").ok();
    match &prev_dir {
        Some(d) => eprintln!("Returned to previous shell. ({d})"),
        None => eprintln!("Returned to previous shell."),
    }

    // SIGKILL terminates the parent shell instantly — no signal handler runs,
    // so no "jobs SIGHUPed" or "hangup" warnings appear.
    unsafe { libc::kill(ppid, libc::SIGKILL); }

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
        "--preview-window=down:40%:wrap".to_string(),
        "--height=85%".to_string(),
        "--border=rounded".to_string(),
        "--header=Claude Code History Browser  [Enter: resume  Ctrl-/: toggle preview  Ctrl-C: cancel]"
            .to_string(),
        "--prompt=Search: ".to_string(),
        "--no-sort".to_string(),
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

    if print_mode {
        let shell_cmd = build_resume_cmd(project, session_id, true, None, None, 0);
        println!("{}", shell_cmd);
    } else {
        let depth = clauhist_depth() + 1;
        let zdotdir = setup_clauhist_zdotdir(depth);
        let prev_dir = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .ok();
        let shell_cmd = build_resume_cmd(
            project,
            session_id,
            false,
            Some(zdotdir.to_str().unwrap()),
            prev_dir.as_deref(),
            depth,
        );
        let _ = Command::new("zsh").arg("-c").arg(&shell_cmd).status();
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
            cmd_preview(&session_id, read_sessions());
        }
        None => {
            let raw = read_sessions();
            let sessions = build_sessions(raw);
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

    fn clauhist_bin() -> PathBuf {
        let mut path = std::env::current_exe().unwrap();
        path.pop(); // test binary name
        path.pop(); // deps/
        path.push("clauhist");
        path
    }

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

    fn unique_temp_path(label: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("clauhist-{label}-{suffix}"))
    }

    #[test]
    fn parse_sessions_skips_blank_invalid_and_missing_session_ids() {
        let raw = r#"
{"sessionId":"alpha","display":"first","timestamp":10,"project":"/tmp/a"}
not json
{"sessionId":"","display":"ignored","timestamp":20,"project":"/tmp/a"}

{"sessionId":"alpha","display":"second","timestamp":30,"project":"/tmp/a"}
"#;

        let sessions = parse_sessions(raw);

        assert_eq!(sessions.len(), 1);
        let alpha = sessions.get("alpha").unwrap();
        assert_eq!(alpha.len(), 2);
        assert_eq!(alpha[0].display.as_deref(), Some("first"));
        assert_eq!(alpha[1].display.as_deref(), Some("second"));
    }

    #[test]
    fn build_sessions_sorts_entries_filters_empty_messages_and_orders_by_recent_activity() {
        let mut raw = HashMap::new();
        raw.insert(
            "older".to_string(),
            vec![
                history_entry("older", Some("later"), Some(30), Some("/tmp/older")),
                history_entry("older", Some(""), Some(20), Some("/tmp/older")),
                history_entry("older", Some("first"), Some(10), Some("/tmp/older")),
            ],
        );
        raw.insert(
            "newer".to_string(),
            vec![history_entry(
                "newer",
                Some("recent"),
                Some(100),
                Some("/tmp/newer"),
            )],
        );

        let sessions = build_sessions(raw);

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "newer");
        assert_eq!(sessions[0].last_ts, 100);

        let older = &sessions[1];
        assert_eq!(older.project, "/tmp/older");
        assert_eq!(older.first_ts, 10);
        assert_eq!(older.last_ts, 30);
        assert_eq!(older.messages.len(), 2);
        assert_eq!(older.messages[0], (10, "first".to_string()));
        assert_eq!(older.messages[1], (30, "later".to_string()));
    }

    #[test]
    fn truncate_respects_character_boundaries() {
        assert_eq!(truncate("こんにちは世界", 4), "こんにち…");
        assert_eq!(truncate("rust", 4), "rust");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/tmp/it's here"), "'/tmp/it'\\''s here'");
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
    fn build_resume_cmd_print_mode_generates_simple_cd_and_resume() {
        let cmd = build_resume_cmd("/tmp/my-project", "abc-123", true, None, None, 0);
        assert_eq!(cmd, "cd '/tmp/my-project' && claude --resume abc-123");
        assert!(!cmd.contains("CLAUHIST_SHELL"));
        assert!(!cmd.contains("exec zsh"));
        assert!(!cmd.contains("ZDOTDIR"));
    }

    #[test]
    fn build_resume_cmd_default_mode_includes_subshell_and_env_var() {
        let cmd = build_resume_cmd("/tmp/my-project", "abc-123", false, Some("/tmp/zd"), Some("/home/user"), 1);
        assert!(cmd.starts_with("cd '/tmp/my-project' && claude --resume abc-123;"));
        assert!(cmd.contains("CLAUHIST_SHELL=1"));
        assert!(cmd.contains("ZDOTDIR='/tmp/zd'"));
        assert!(cmd.contains("CLAUHIST_PREV_DIR='/home/user'"));
        assert!(cmd.contains("go back to /home/user"));
        assert!(cmd.contains("exec zsh -i"));
        assert!(cmd.contains("clauhist --return"));
    }

    #[test]
    fn build_resume_cmd_nested_depth_is_reflected() {
        let cmd = build_resume_cmd("/tmp/p", "s1", false, None, None, 3);
        assert!(cmd.contains("CLAUHIST_SHELL=3"));
    }

    #[test]
    fn build_resume_cmd_default_mode_without_zdotdir() {
        let cmd = build_resume_cmd("/tmp/my-project", "abc-123", false, None, None, 1);
        assert!(cmd.contains("CLAUHIST_SHELL=1 exec zsh -i"));
        assert!(!cmd.contains("ZDOTDIR"));
        assert!(cmd.contains("go back."));
    }

    #[test]
    fn build_resume_cmd_quotes_project_path_with_special_chars() {
        let cmd = build_resume_cmd("/tmp/it's here", "sess-1", true, None, None, 0);
        assert_eq!(cmd, "cd '/tmp/it'\\''s here' && claude --resume sess-1");

        let cmd = build_resume_cmd("/tmp/it's here", "sess-1", false, None, None, 1);
        assert!(cmd.starts_with("cd '/tmp/it'\\''s here' && claude --resume sess-1;"));
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
    fn cmd_init_zsh_output_contains_print_flag() {
        let output = std::process::Command::new(clauhist_bin().to_str().unwrap())
            .args(["init", "zsh"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("--print"), "shell integration must pass --print to binary");
        assert!(stdout.contains("eval"), "shell integration must eval the output");
    }

    #[test]
    fn cmd_init_bash_output_contains_print_flag() {
        let output = std::process::Command::new(clauhist_bin().to_str().unwrap())
            .args(["init", "bash"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("--print"), "shell integration must pass --print to binary");
        assert!(stdout.contains("eval"), "shell integration must eval the output");
    }

    #[test]
    fn cmd_init_fish_output_contains_print_flag() {
        let output = std::process::Command::new(clauhist_bin().to_str().unwrap())
            .args(["init", "fish"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("--print"), "shell integration must pass --print to binary");
        assert!(stdout.contains("eval"), "shell integration must eval the output");
    }

    #[test]
    fn return_flag_outside_clauhist_shell_exits_with_error() {
        let output = std::process::Command::new(clauhist_bin().to_str().unwrap())
            .arg("--return")
            .env_remove("CLAUHIST_SHELL")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Not inside a clauhist sub-shell"));
    }

    #[test]
    fn render_preview_formats_metadata_and_message_lines() {
        let invalid_ms = i64::MAX as u64;
        let preview = render_preview(&Session {
            session_id: "session-1".to_string(),
            project: "/tmp/example".to_string(),
            first_ts: invalid_ms,
            last_ts: invalid_ms,
            messages: vec![(invalid_ms, "line one\nline two".to_string())],
        });

        assert!(preview.contains("Project : /tmp/example"));
        assert!(preview.contains("Session : session-1"));
        assert!(preview.contains("Started : unknown"));
        assert!(preview.contains("Last    : unknown"));
        assert!(preview.contains("Messages: 1"));
        assert!(preview.contains("[??:??] line one line two"));
    }
}

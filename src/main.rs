use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use chrono::Local;
use clap::{Parser, Subcommand};
use serde::Deserialize;

#[derive(Deserialize)]
struct HistoryEntry {
    #[serde(rename = "sessionId", default)]
    session_id: String,
    display: Option<String>,
    timestamp: Option<u64>, // milliseconds since epoch
    project: Option<String>,
}

struct Session {
    session_id: String,
    project: String,
    first_ts: u64,
    last_ts: u64,
    messages: Vec<(u64, String)>, // (timestamp_ms, display)
}

#[derive(Parser)]
#[command(name = "clauhist", about = "Browse and resume Claude Code chat sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Print shell command to stdout instead of spawning a subshell
    #[arg(long)]
    print: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Show session preview (used internally by fzf --preview)
    Preview { session_id: String },
}

fn history_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| {
        eprintln!("HOME environment variable not set");
        std::process::exit(1);
    });
    PathBuf::from(home).join(".claude").join("history.jsonl")
}

fn read_sessions() -> HashMap<String, Vec<HistoryEntry>> {
    let path = history_file();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let mut sessions: HashMap<String, Vec<HistoryEntry>> = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<HistoryEntry>(line) {
            if !entry.session_id.is_empty() {
                sessions.entry(entry.session_id.clone()).or_default().push(entry);
            }
        }
    }
    sessions
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
            Session { session_id, project, first_ts, last_ts, messages }
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
        .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
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
            let exists = if std::path::Path::new(&s.project).exists() { "✓" } else { "✗" };
            let first_msg = s
                .messages
                .first()
                .map(|(_, msg)| truncate(&msg.replace(['\t', '\n'], " "), 70))
                .unwrap_or_default();
            format!(
                "{}\t{}\t{} {}\t{}\t({})",
                s.session_id, date_str, exists, s.project, first_msg, s.messages.len()
            )
        })
        .collect()
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
    println!("Project : {}", session.project);
    println!("Session : {}", session.session_id);
    println!("Started : {}", format_ts_datetime(session.first_ts));
    println!("Last    : {}", format_ts_datetime(session.last_ts));
    println!("Messages: {}", session.messages.len());
    println!("{}", "─".repeat(60));
    for (ts, msg) in &session.messages {
        let clean = msg.replace('\n', " ");
        println!("[{}] {}", format_ts_time(*ts), truncate(&clean, 120));
    }
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
    let shell_cmd = format!("cd {} && claude --resume {}", shell_quote(project), session_id);

    if print_mode {
        println!("{}", shell_cmd);
    } else {
        let _ = Command::new("zsh").arg("-c").arg(&shell_cmd).status();
    }
}

fn main() {
    let cli = Cli::parse();

    let exe_path = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "clauhist".to_string());

    match cli.command {
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

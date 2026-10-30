//! Read local Codex rollouts; history.jsonl alone does not contain the cwd.
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::Session;

pub(super) fn config_dir() -> PathBuf {
    crate::Source::Codex
        .config_override()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").expect("HOME environment variable must be set"))
                .join(".codex")
        })
}

fn timestamp(value: &Value) -> u64 {
    value
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .and_then(|dt| u64::try_from(dt.timestamp_millis()).ok())
        .unwrap_or(0)
}

fn parse_rollout(reader: impl BufRead) -> Option<Session> {
    let mut session = None;
    let mut messages = Vec::new();
    let mut legacy_messages = Vec::new();
    let mut last_ts = 0;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            // A running session can have an incomplete final line.
            continue;
        };
        let ts = timestamp(&value["timestamp"]);
        last_ts = last_ts.max(ts);
        let payload = &value["payload"];
        match value["type"].as_str() {
            Some("session_meta") if session.is_none() => {
                let id = payload["id"].as_str()?;
                let project = payload["cwd"].as_str()?;
                if id.is_empty() || project.is_empty() {
                    return None;
                }
                // Internal agents are not standalone user conversations.
                if payload["source"].get("subagent").is_some()
                    || payload["source"].get("internal").is_some()
                    || payload["thread_source"]
                        .as_str()
                        .is_some_and(|s| s != "user")
                {
                    return None;
                }
                session = Some(Session {
                    session_id: id.to_owned(),
                    project: project.to_owned(),
                    first_ts: match timestamp(&payload["timestamp"]) {
                        0 => ts,
                        started => started,
                    },
                    last_ts: 0,
                    messages: Vec::new(),
                });
            }
            Some("event_msg") if payload["type"] == "user_message" => {
                if let Some(message) = payload["message"].as_str().filter(|s| !s.is_empty()) {
                    messages.push((ts, message.to_owned()));
                }
            }
            // Older rollouts only persisted response items. Prefer user_message
            // events when available, since the same prompt appears in both.
            Some("response_item") if payload["role"] == "user" => {
                if let Some(content) = payload["content"].as_array() {
                    let text = content
                        .iter()
                        .filter(|part| part["type"] == "input_text")
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty()
                        && !text.starts_with("# AGENTS.md instructions")
                        && !text.starts_with("<environment_context>")
                    {
                        legacy_messages.push((ts, text));
                    }
                }
            }
            _ => {}
        }
    }
    let mut session = session?;
    session.messages = if messages.is_empty() {
        legacy_messages
    } else {
        messages
    };
    session.messages.sort_by_key(|(ts, _)| *ts);
    session.last_ts = last_ts.max(session.first_ts);
    Some(session)
}

fn rollout_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Do not follow symlinks out of the sessions tree.
        if kind.is_dir() {
            rollout_files(&entry.path(), files);
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "jsonl") {
            files.push(entry.path());
        }
    }
}

pub(super) fn read_sessions(config_dir: &Path, session_id: Option<&str>) -> Vec<Session> {
    let mut files = Vec::new();
    rollout_files(&config_dir.join("sessions"), &mut files);
    let mut sessions = HashMap::<String, Session>::new();
    for path in files {
        // Codex includes the ID in rollout filenames; preview should not read
        // every other conversation on each cursor movement.
        if session_id.is_some_and(|id| !path.to_string_lossy().contains(id)) {
            continue;
        }
        let Ok(file) = File::open(path) else { continue };
        if let Some(session) = parse_rollout(BufReader::new(file)) {
            if session_id.is_some_and(|id| session.session_id != id) {
                continue;
            }
            let existing = sessions
                .entry(session.session_id.clone())
                .or_insert_with(|| Session {
                    session_id: session.session_id.clone(),
                    project: session.project.clone(),
                    first_ts: session.first_ts,
                    last_ts: 0,
                    messages: Vec::new(),
                });
            // If a resumed session has multiple rollouts, retain the most
            // recent one rather than duplicating its prompt history.
            if session.last_ts >= existing.last_ts {
                *existing = session;
            }
        }
    }
    let mut sessions: Vec<_> = sessions.into_values().collect();
    sessions.sort_by(|a, b| {
        b.last_ts
            .cmp(&a.last_ts)
            .then(a.session_id.cmp(&b.session_id))
    });
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rollout(extra: &[Value]) -> Option<Session> {
        let mut content = format!(
            "{}\n",
            json!({
                "timestamp": "2026-10-30T01:00:00Z", "type": "session_meta",
                "payload": {"id":"s1", "cwd":"/tmp/project", "source":"cli"}
            })
        );
        for value in extra {
            content.push_str(&format!("{value}\n"));
        }
        content.push_str("{unfinished");
        parse_rollout(std::io::Cursor::new(content))
    }

    #[test]
    fn prefers_user_events_over_duplicated_response_items_and_tracks_last_activity() {
        let session = rollout(&[
            json!({"timestamp":"2026-10-30T01:01:00Z", "type":"response_item", "payload":{
                "type":"message", "role":"user", "content":[{"type":"input_text", "text":"hello"}]}}),
            json!({"timestamp":"2026-10-30T01:01:00Z", "type":"event_msg", "payload":{
                "type":"user_message", "message":"hello"}}),
            json!({"timestamp":"2026-10-30T01:02:00Z", "type":"event_msg", "payload":{
                "type":"agent_message", "message":"answer"}}),
        ]).unwrap();
        assert_eq!(session.messages, vec![(1793322060000, "hello".into())]);
        assert_eq!(session.last_ts - session.first_ts, 120_000);
    }

    #[test]
    fn legacy_rollouts_skip_injected_context_and_join_prompt_parts() {
        let session = rollout(&[
            json!({"type":"response_item", "payload":{"role":"user", "content":[
                {"type":"input_text", "text":"# AGENTS.md instructions for /tmp"}]}}),
            json!({"type":"response_item", "payload":{"role":"user", "content":[
                {"type":"input_text", "text":"<environment_context>cwd</environment_context>"}]}}),
            json!({"type":"response_item", "payload":{"role":"user", "content":[
                {"type":"input_text", "text":"hello"}, {"type":"input_image", "image_url":"..."},
                {"type":"input_text", "text":"world"}]}}),
        ])
        .unwrap();
        assert_eq!(session.messages, vec![(0, "hello\nworld".into())]);
    }

    #[test]
    fn skips_missing_metadata_and_internal_agents() {
        assert!(parse_rollout(std::io::Cursor::new("not json\n")).is_none());
        for payload in [
            json!({"id":"s1"}),
            json!({"id":"", "cwd":"/tmp"}),
            json!({"id":"s1", "cwd":"/tmp", "source":{"subagent":{"spawn":{}}}}),
            json!({"id":"s1", "cwd":"/tmp", "thread_source":"subagent"}),
        ] {
            assert!(parse_rollout(std::io::Cursor::new(format!(
                "{}\n",
                json!({"type":"session_meta", "payload":payload})
            )))
            .is_none());
        }
    }
}

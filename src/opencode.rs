//! OpenCode's SQLite storage is read directly, without starting its server.
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::Session;

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub(super) fn database_path() -> PathBuf {
    let data = env_path("XDG_DATA_HOME")
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME environment variable must be set"))
                .join(".local/share")
        })
        .join("opencode");
    match env_path("OPENCODE_DB") {
        Some(path) if path.is_absolute() || path == Path::new(":memory:") => path,
        Some(path) => data.join(path),
        None => data.join("opencode.db"),
    }
}

pub(super) fn config_override() -> Option<String> {
    // Resolve a relative override before changing to the session's cwd.
    env_path("OPENCODE_DB").map(|_| database_path().to_string_lossy().into_owned())
}

fn has_table(db: &Connection, name: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |row| row.get(0),
    )
}

fn read_database(db: &Connection, session_id: Option<&str>) -> rusqlite::Result<Vec<Session>> {
    // Upgraded installations retain V1 tables; prefer the active V2 sessions.
    let v2 = has_table(db, "session_v2")?;
    let table = if v2 { "session_v2" } else { "session" };
    if !has_table(db, table)? {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut statement = db.prepare(&format!(
        "SELECT id, directory, time_created, time_updated FROM {table}
         WHERE parent_id IS NULL AND time_archived IS NULL
         AND (?1 IS NULL OR id = ?1) ORDER BY time_updated DESC, id"
    ))?;
    let sessions = statement
        .query_map([session_id], |row| {
            Ok(Session {
                session_id: row.get(0)?,
                project: row.get(1)?,
                first_ts: row.get::<_, i64>(2)?.max(0) as u64,
                last_ts: row.get::<_, i64>(3)?.max(0) as u64,
                messages: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut result = Vec::new();
    for mut session in sessions {
        // Empty directories represent sessions without a local working tree.
        if session.project.is_empty() || session.session_id.is_empty() {
            continue;
        }
        session.messages = if v2 {
            read_v2_messages(db, &session.session_id)?
        } else {
            read_v1_messages(db, &session.session_id)?
        };
        result.push(session);
    }
    Ok(result)
}

fn read_v2_messages(db: &Connection, id: &str) -> rusqlite::Result<Vec<(u64, String)>> {
    let mut statement = db.prepare(
        "SELECT time_created, data FROM session_message
         WHERE session_id = ?1 AND type = 'user' ORDER BY seq, id",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, i64>(0)?.max(0) as u64,
            row.get::<_, String>(1)?,
        ))
    })?;
    let mut messages = Vec::new();
    for row in rows {
        let (ts, data) = row?;
        if let Ok(value) = serde_json::from_str::<Value>(&data) {
            if let Some(text) = value["text"].as_str().filter(|s| !s.is_empty()) {
                messages.push((ts, text.to_owned()));
            }
        }
    }
    Ok(messages)
}

fn read_v1_messages(db: &Connection, id: &str) -> rusqlite::Result<Vec<(u64, String)>> {
    let mut statement = db.prepare(
        "SELECT m.id, m.time_created, p.data FROM message m
         LEFT JOIN part p ON p.message_id = m.id AND p.session_id = m.session_id
         WHERE m.session_id = ?1
         AND CASE WHEN json_valid(m.data) THEN json_extract(m.data, '$.role') = 'user' ELSE 0 END
         ORDER BY m.time_created, m.id, p.time_created, p.id",
    )?;
    let rows = statement.query_map([id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?.max(0) as u64,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut messages: Vec<(u64, String)> = Vec::new();
    let mut last_id = String::new();
    for row in rows {
        let (message_id, ts, part) = row?;
        let Some(part) = part.and_then(|p| serde_json::from_str::<Value>(&p).ok()) else {
            continue;
        };
        if part["type"] != "text" || part["synthetic"] == true || part["ignored"] == true {
            continue;
        }
        let Some(text) = part["text"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if message_id == last_id {
            let (_, message) = messages.last_mut().expect("a previous part exists");
            message.push('\n');
            message.push_str(text);
        } else {
            messages.push((ts, text.to_owned()));
            last_id = message_id;
        }
    }
    Ok(messages)
}

pub(super) fn read_sessions(path: &Path, session_id: Option<&str>) -> Result<Vec<Session>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    // Never create or migrate a database. Normal SQLite locking also includes
    // committed WAL data from a concurrently running OpenCode instance.
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("Cannot read OpenCode database {}: {e}", path.display()))?;
    db.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    read_database(&db, session_id)
        .map_err(|e| format!("Cannot read OpenCode history from {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn schema(db: &Connection, v2: bool) {
        let table = if v2 { "session_v2" } else { "session" };
        db.execute_batch(&format!(
            "CREATE TABLE {table} (id TEXT PRIMARY KEY, directory TEXT, parent_id TEXT,
             time_created INTEGER, time_updated INTEGER, time_archived INTEGER);"
        ))
        .unwrap();
        if v2 {
            db.execute_batch(
                "CREATE TABLE session_message (id TEXT, session_id TEXT,
                type TEXT, seq INTEGER, time_created INTEGER, data TEXT);",
            )
            .unwrap();
        } else {
            db.execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
                CREATE TABLE part (id TEXT, session_id TEXT, message_id TEXT, time_created INTEGER, data TEXT);").unwrap();
        }
    }

    #[test]
    fn v2_reads_ordered_prompts_filters_non_user_sessions_and_prefers_active_schema() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db, true);
        schema(&db, false);
        db.execute_batch(
            "INSERT INTO session_v2 VALUES
            ('older','/tmp/old',NULL,10,20,NULL),
            ('newer','/tmp/new',NULL,10,50,NULL),
            ('child','/tmp/new','newer',10,60,NULL),
            ('archived','/tmp/new',NULL,10,70,70),
            ('remote','',NULL,10,80,NULL);
            INSERT INTO session VALUES ('legacy','/tmp/old',NULL,1,100,NULL);",
        )
        .unwrap();
        for (id, kind, seq, data) in [
            ("msg_2", "user", 3, r#"{"text":"second"}"#),
            ("msg_1", "user", 1, r#"{"text":"最初の質問"}"#),
            ("msg_a", "assistant", 2, r#"{"text":"answer"}"#),
            ("msg_s", "synthetic", 4, r#"{"text":"internal prompt"}"#),
            ("msg_bad", "user", 5, "{unfinished"),
        ] {
            db.execute(
                "INSERT INTO session_message VALUES (?1,'newer',?2,?3,?3,?4)",
                params![id, kind, seq, data],
            )
            .unwrap();
        }
        let sessions = read_database(&db, None).unwrap();
        assert_eq!(
            sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            ["newer", "older"]
        );
        assert_eq!(
            sessions[0].messages,
            vec![(1, "最初の質問".into()), (3, "second".into())]
        );
        assert_eq!(read_database(&db, Some("older")).unwrap().len(), 1);
        assert!(read_database(&db, Some("missing")).unwrap().is_empty());
    }

    #[test]
    fn v1_combines_text_parts_and_ignores_assistant_synthetic_and_ignored_parts() {
        let db = Connection::open_in_memory().unwrap();
        schema(&db, false);
        db.execute_batch(
            "INSERT INTO session VALUES ('s1','/tmp/project',NULL,1,10,NULL);
            INSERT INTO message VALUES ('m1','s1',2,'{\"role\":\"user\"}'),
                ('m2','s1',3,'{\"role\":\"assistant\"}');",
        )
        .unwrap();
        for (id, msg, data) in [
            ("p1", "m1", r#"{"type":"text","text":"one"}"#),
            ("p2", "m1", r#"{"type":"text","text":"two"}"#),
            (
                "p3",
                "m1",
                r#"{"type":"text","text":"injected","synthetic":true}"#,
            ),
            (
                "p4",
                "m1",
                r#"{"type":"text","text":"ignored","ignored":true}"#,
            ),
            ("p5", "m2", r#"{"type":"text","text":"answer"}"#),
        ] {
            db.execute(
                "INSERT INTO part VALUES (?1,'s1',?2,2,?3)",
                params![id, msg, data],
            )
            .unwrap();
        }
        let sessions = read_database(&db, Some("s1")).unwrap();
        assert_eq!(sessions[0].messages, vec![(2, "one\ntwo".into())]);
    }

    #[test]
    fn missing_database_is_not_created() {
        let path = std::env::temp_dir().join(format!(
            "clauhist-opencode-missing-{}.db",
            std::process::id()
        ));
        assert!(read_sessions(&path, None).unwrap().is_empty());
        assert!(!path.exists());
    }
}

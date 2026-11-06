use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Claude Code names a session twice: once from the model, once from the user's
/// `/rename`. Both land in the session transcript as their own record.
#[derive(Debug, Deserialize)]
struct TitleRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId", default)]
    session_id: String,
    #[serde(rename = "customTitle")]
    custom: Option<String>,
    #[serde(rename = "aiTitle")]
    ai: Option<String>,
}

const CUSTOM_KIND: &str = "custom-title";
const AI_KIND: &str = "ai-title";

/// Both record types end on this, and no other key in a transcript does. A
/// transcript is mostly assistant turns and tool output, so skipping the JSON
/// parse for lines that cannot be a title is what keeps the scan cheap.
const MARKER: &str = "-title\"";

/// Claude Code rewrites both records on every turn, so the current name sits at
/// the end of the transcript: across 377MB of named transcripts here, the last
/// one was never more than 31KB from the end. Reading a window instead of the
/// file turns hundreds of megabytes into a few, and a window far larger than
/// the observed distance costs nothing to read.
const TAIL: u64 = 256 * 1024;

/// Adds the session's name to each fzf line as a column of its own, after the
/// project. fzf matches on the columns it displays, so a name has to be in the
/// line to be searchable; a session without one keeps an empty column and
/// reads as it did before.
pub fn insert_names(lines: &mut [String], config_dir: &Path) {
    let titles = read_titles(&config_dir.join("projects"));
    for line in lines.iter_mut() {
        let session_id = line.split('\t').next().unwrap_or_default();
        let name = titles.get(session_id).cloned().unwrap_or_default();
        let mut fields: Vec<&str> = line.split('\t').collect();
        fields.insert(NAME_FIELD - 1, &name);
        *line = fields.join("\t");
    }
}

/// 1-based, the way fzf counts `--with-nth`: after the session id, date and
/// project, and ahead of the first message.
const NAME_FIELD: usize = 4;

/// Session names by session id, preferring the user's `/rename` over the
/// generated name the way Claude Code's own session list does.
fn read_titles(projects: &Path) -> HashMap<String, String> {
    let mut names = Names::default();
    for file in transcripts(projects) {
        read_file(&file, TAIL, &mut names);
    }
    names.ai.extend(names.custom);
    names.ai
}

#[derive(Default)]
struct Names {
    ai: HashMap<String, String>,
    custom: HashMap<String, String>,
}

/// The window is where the name is expected, not where it has to be: a session
/// whose last turns were one huge tool result, or a transcript from a Claude
/// Code that wrote the record once, still gets its name from the whole file.
fn read_file(file: &Path, tail: u64, names: &mut Names) {
    if let Some(window) = read_tail(file, tail) {
        if window.contains(MARKER) {
            scan(&window, names);
            return;
        }
    }
    let Ok(content) = std::fs::read_to_string(file) else {
        return;
    };
    scan(&content, names);
}

/// The last `tail` bytes, minus the line the window opens in the middle of.
/// `None` for a file that is no larger than the window, which is read whole.
fn read_tail(file: &Path, tail: u64) -> Option<String> {
    let mut handle = std::fs::File::open(file).ok()?;
    if handle.metadata().ok()?.len() <= tail {
        return None;
    }
    handle.seek(SeekFrom::End(-(tail as i64))).ok()?;
    let mut window = Vec::new();
    handle.read_to_end(&mut window).ok()?;
    let text = String::from_utf8_lossy(&window).into_owned();
    // A half line is not a record, and a record cut in half is not a name.
    Some(text.split_once('\n')?.1.to_string())
}

fn scan(content: &str, names: &mut Names) {
    for line in content.lines().filter(|line| line.contains(MARKER)) {
        let Ok(record) = serde_json::from_str::<TitleRecord>(line) else {
            continue;
        };
        if record.session_id.is_empty() {
            continue;
        }
        // A rename recorded after another rename wins; a generated name never
        // overrides a rename, however late it arrives — and it does arrive
        // later, since Claude Code keeps writing both records to the end.
        match (record.kind.as_str(), record.custom, record.ai) {
            (CUSTOM_KIND, Some(title), _) => names.custom.insert(record.session_id, title),
            (AI_KIND, _, Some(title)) => names.ai.insert(record.session_id, title),
            _ => None,
        };
    }
}

/// Every `projects/<project>/<session id>.jsonl`. Subagent transcripts sit one
/// level deeper and carry no title records, so one level is enough.
fn transcripts(projects: &Path) -> Vec<PathBuf> {
    let Ok(dirs) = std::fs::read_dir(projects) else {
        return Vec::new();
    };
    dirs.flatten()
        .filter_map(|dir| std::fs::read_dir(dir.path()).ok())
        .flat_map(|files| files.flatten())
        .map(|file| file.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A config directory laid out the way Claude Code lays out its own, so a
    /// test can hand either it or its `projects` to the code under test.
    fn temp_config(label: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("clauhist-titles-{label}-{id}"));
        std::fs::create_dir_all(dir.join("projects").join("-home-user-project")).unwrap();
        dir
    }

    fn write_transcript(config: &Path, session_id: &str, lines: &[&str]) -> PathBuf {
        let file = config
            .join("projects")
            .join("-home-user-project")
            .join(format!("{session_id}.jsonl"));
        std::fs::write(&file, format!("{}\n", lines.join("\n"))).unwrap();
        file
    }

    fn ai(session_id: &str, title: &str) -> String {
        format!(r#"{{"type":"ai-title","aiTitle":"{title}","sessionId":"{session_id}"}}"#)
    }

    fn custom(session_id: &str, title: &str) -> String {
        format!(r#"{{"type":"custom-title","customTitle":"{title}","sessionId":"{session_id}"}}"#)
    }

    #[test]
    fn read_titles_prefers_the_rename_over_a_later_generated_name() {
        let config = temp_config("rename");
        write_transcript(
            &config,
            "alpha",
            &[
                &ai("alpha", "Generated first"),
                &custom("alpha", "backlog"),
                &custom("alpha", "baklog"),
                // Claude Code writes this after the rename on every turn.
                &ai("alpha", "Generated later"),
            ],
        );

        let titles = read_titles(&config.join("projects"));

        assert_eq!(titles.get("alpha"), Some(&"baklog".to_string()));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn read_titles_falls_back_to_the_generated_name_and_takes_the_last_one() {
        let config = temp_config("generated");
        write_transcript(
            &config,
            "beta",
            &[&ai("beta", "First guess"), &ai("beta", "Better guess")],
        );

        let titles = read_titles(&config.join("projects"));

        assert_eq!(titles.get("beta"), Some(&"Better guess".to_string()));
        std::fs::remove_dir_all(config).unwrap();
    }

    /// A session that talks about the record format must not be named by its
    /// own transcript: the marker appears in plain user text too.
    #[test]
    fn read_titles_ignores_records_that_only_mention_a_title() {
        let config = temp_config("mention");
        write_transcript(
            &config,
            "gamma",
            &[
                r#"{"type":"user","sessionId":"gamma","message":{"role":"user","content":"grep for \"type\":\"ai-title\" in the transcripts"}}"#,
                r#"{"type":"assistant","sessionId":"gamma","customTitle":"not a title record"}"#,
            ],
        );

        let titles = read_titles(&config.join("projects"));

        assert!(titles.is_empty());
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn read_tail_opens_the_window_on_a_line_boundary() {
        let config = temp_config("window");
        let padding = "x".repeat(500);
        let file = write_transcript(
            &config,
            "delta",
            &[&ai("delta", "Named long ago"), &padding, &padding, &padding],
        );

        let window = read_tail(&file, 800).unwrap();

        // Whatever the window cut in half is gone, so no line is a fragment
        // and no record can be half-read.
        assert!(window.lines().all(|line| line == padding));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn read_tail_leaves_a_file_that_fits_in_the_window_to_be_read_whole() {
        let config = temp_config("small");
        let file = write_transcript(&config, "delta", &[&ai("delta", "Short session")]);

        assert!(read_tail(&file, TAIL).is_none());
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn read_file_finds_a_name_in_a_transcript_larger_than_the_window() {
        let config = temp_config("tail");
        let file = write_transcript(
            &config,
            "zeta",
            &["z".repeat(8192).as_str(), &ai("zeta", "Named just now")],
        );
        let mut names = Names::default();

        read_file(&file, 1024, &mut names);

        assert_eq!(names.ai.get("zeta"), Some(&"Named just now".to_string()));
        std::fs::remove_dir_all(config).unwrap();
    }

    /// The window is an expectation, not a contract: a name further back than
    /// the window still has to be found.
    #[test]
    fn read_file_falls_back_to_the_whole_file_when_the_window_has_no_title() {
        let config = temp_config("fallback");
        let file = write_transcript(
            &config,
            "epsilon",
            &[&ai("epsilon", "Named long ago"), &"y".repeat(8192)],
        );
        let mut names = Names::default();

        read_file(&file, 1024, &mut names);

        assert_eq!(names.ai.get("epsilon"), Some(&"Named long ago".to_string()));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn read_titles_is_empty_without_a_projects_directory() {
        assert!(read_titles(&std::env::temp_dir().join("clauhist-titles-missing")).is_empty());
    }

    #[test]
    fn insert_names_adds_an_empty_column_for_an_unnamed_session() {
        let config = temp_config("insert");
        write_transcript(&config, "alpha", &[&custom("alpha", "baklog")]);
        let mut lines = vec![
            "alpha\t2026-10-31 12:00\t✓ /home/user/project\thello\t(3)".to_string(),
            "beta\t2026-10-30 09:00\t✓ /home/user/other\thi\t(1)".to_string(),
        ];

        insert_names(&mut lines, &config);

        assert_eq!(
            lines[0],
            "alpha\t2026-10-31 12:00\t✓ /home/user/project\tbaklog\thello\t(3)"
        );
        assert_eq!(
            lines[1],
            "beta\t2026-10-30 09:00\t✓ /home/user/other\t\thi\t(1)"
        );
        // fzf's --with-nth has to list every field a line now reaches.
        assert_eq!(lines[0].split('\t').count(), 6);
        std::fs::remove_dir_all(config).unwrap();
    }
}

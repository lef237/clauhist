//! End-to-end tests that drive the compiled `clauhist` binary.
//!
//! These live in `tests/` on purpose: Cargo builds the binary before running
//! integration tests and exposes its path through `CARGO_BIN_EXE_clauhist`, so
//! they always exercise the current code. Keeping them in the unit-test module
//! instead meant reaching into `target/debug/clauhist`, which `cargo test` does
//! not rebuild — tests could silently run against a stale binary (or fail when
//! it had never been built).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_clauhist");

fn home_path(relative: &str) -> String {
    let home = std::env::var("HOME").unwrap();
    format!("{home}/{relative}")
}

fn unique_temp_path(label: &str) -> PathBuf {
    // Tests run in parallel and the clock can report the same nanos twice, so a
    // counter is what actually keeps two callers apart: sharing a directory
    // means one test deletes the stub another one is about to run.
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("clauhist-{label}-{suffix}-{seq}"))
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn run_init(shell: &str) -> String {
    let output = std::process::Command::new(BIN)
        .args(["init", shell])
        .output()
        .unwrap();
    assert!(output.status.success(), "init {shell} should succeed");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn preview_reads_history_from_claude_config_dir() {
    let config_dir = unique_temp_path("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let p = home_path("projects/relocated");
    std::fs::write(
        config_dir.join("history.jsonl"),
        format!(
            "{{\"sessionId\":\"relocated-1\",\"display\":\"hello from CLAUDE_CONFIG_DIR\",\"timestamp\":10,\"project\":\"{p}\"}}\n"
        ),
    )
    .unwrap();

    let output = std::process::Command::new(BIN)
        .args(["preview", "relocated-1"])
        .env("CLAUDE_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello from CLAUDE_CONFIG_DIR"),
        "history must be read from $CLAUDE_CONFIG_DIR, got: {stdout:?}"
    );

    std::fs::remove_dir_all(config_dir).unwrap();
}

#[test]
fn history_falls_back_to_home_when_config_dir_is_unset_or_empty() {
    let fake_home = unique_temp_path("home");
    std::fs::create_dir_all(fake_home.join(".claude")).unwrap();
    let p = home_path("projects/plain");
    std::fs::write(
        fake_home.join(".claude").join("history.jsonl"),
        format!(
            "{{\"sessionId\":\"home-1\",\"display\":\"hello from HOME\",\"timestamp\":10,\"project\":\"{p}\"}}\n"
        ),
    )
    .unwrap();

    for config_dir in [None, Some("")] {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(["preview", "home-1"]).env("HOME", &fake_home);
        match config_dir {
            Some(v) => cmd.env("CLAUDE_CONFIG_DIR", v),
            None => cmd.env_remove("CLAUDE_CONFIG_DIR"),
        };

        let output = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hello from HOME"),
            "CLAUDE_CONFIG_DIR={config_dir:?} must fall back to $HOME/.claude, got: {stdout:?}"
        );
    }

    std::fs::remove_dir_all(fake_home).unwrap();
}

#[test]
fn return_flag_refuses_when_parent_is_not_the_recorded_subshell() {
    let bin = shell_quote(BIN);
    // Run through an extra sh so a regression signals that throwaway shell
    // instead of the test runner.
    for recorded in [Some("999999"), None] {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("{bin} --return; echo \"rc=$?\""))
            .env("CLAUHIST_SHELL", "1");
        match recorded {
            Some(pid) => cmd.env("CLAUHIST_SHELL_PID", pid),
            None => cmd.env_remove("CLAUHIST_SHELL_PID"),
        };

        let output = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stderr.contains("only works directly in the clauhist sub-shell"),
            "CLAUHIST_SHELL_PID={recorded:?} must be refused, got: {stderr:?}"
        );
        assert!(
            stdout.contains("rc=1"),
            "expected exit status 1, got: {stdout:?}"
        );
    }
}

/// Happy path: the recorded shell is hung up (so it saves its history)
/// rather than killed, and it stops running the rest of its input.
#[test]
fn return_flag_hangs_up_the_recorded_subshell() {
    use std::os::unix::process::ExitStatusExt;

    // SIGHUP is 1 on every Unix clauhist supports.
    const SIGHUP: i32 = 1;

    let marker = unique_temp_path("marker");
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    let inner = format!(
        "{bin} --return; sleep 1; : > {marker}",
        bin = shell_quote(BIN),
        marker = shell_quote(&marker.to_string_lossy()),
    );
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "CLAUHIST_SHELL=1 CLAUHIST_SHELL_PID=$$ exec sh -c {}",
            shell_quote(&inner)
        ))
        .output()
        .unwrap();
    let status = output.status;

    assert_eq!(
        status.signal(),
        Some(SIGHUP),
        "sub-shell should be hung up, not killed: {status:?}"
    );
    assert!(!marker.exists(), "sub-shell kept running after --return");
}

#[test]
fn return_flag_outside_clauhist_shell_exits_with_error() {
    let output = std::process::Command::new(BIN)
        .arg("--return")
        .env_remove("CLAUHIST_SHELL")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Not inside a clauhist sub-shell"));
}

#[test]
fn cmd_init_unsupported_shell_lists_nu_as_supported() {
    let output = std::process::Command::new(BIN)
        .args(["init", "tcsh"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nu"),
        "error message must advertise nu as a supported shell"
    );
}

#[test]
fn cmd_init_zsh_wrapper_consumes_three_line_contract() {
    let stdout = run_init("zsh");
    assert!(stdout.contains("--print"));
    assert!(
        stdout.contains(r#"cd -- "$project""#),
        "must cd safely with --"
    );
    assert!(
        stdout.contains(r#"CLAUDE_CONFIG_DIR="$cfg" claude --resume "$sid""#),
        "must resume under the config directory from the contract"
    );
    assert!(
        stdout.contains(r#"elif [[ -n "$cfg" ]]; then"#),
        "must set CLAUDE_CONFIG_DIR only when the contract carried one"
    );
    assert!(
        stdout.contains(r#"[[ "$out" == *$'\n'* ]] || return"#),
        "must reject output with fewer than two lines"
    );
    assert!(
        !stdout.contains("eval"),
        "no more eval; shell formats cd itself"
    );
}

#[test]
fn cmd_init_bash_wrapper_consumes_three_line_contract() {
    let stdout = run_init("bash");
    assert!(stdout.contains("--print"));
    assert!(stdout.contains(r#"cd -- "$project""#));
    assert!(stdout.contains(r#"CLAUDE_CONFIG_DIR="$cfg" claude --resume "$sid""#));
    assert!(stdout.contains(r#"elif [[ -n "$cfg" ]]; then"#));
    assert!(stdout.contains(r#"[[ "$out" == *$'\n'* ]] || return"#));
    assert!(!stdout.contains("eval"));
}

#[test]
fn cmd_init_fish_wrapper_consumes_three_line_contract() {
    let stdout = run_init("fish");
    assert!(stdout.contains("--print"));
    // fish's `cd` builtin rejects `--`, so we trust the variable expansion alone.
    assert!(stdout.contains("cd $out[1]"));
    assert!(
        stdout.contains("env CLAUDE_CONFIG_DIR=$out[3] claude --resume $out[2]"),
        "fish has no inline assignment, so the config dir goes through env"
    );
    assert!(
        stdout.contains("test $n -lt 2 -o $n -gt 3"),
        "must require two or three lines"
    );
    assert!(
        stdout.contains("test $n -eq 3"),
        "must set the config dir only when the contract carried one"
    );
    assert!(!stdout.contains("eval"));
}

#[test]
fn cmd_init_nu_wrapper_consumes_three_line_contract() {
    let stdout = run_init("nu");
    assert!(stdout.contains("--print"));
    assert!(
        stdout.contains("def --env clauhist"),
        "must opt into env mutation so cd propagates"
    );
    assert!(
        stdout.contains("print --stderr $result.stderr"),
        "must surface failures instead of returning silently"
    );
    assert!(stdout.contains("cd ($lines | get 0)"));
    assert!(stdout.contains("^claude --resume ($lines | get 1)"));
    assert!(
        stdout.contains("with-env { CLAUDE_CONFIG_DIR: ($lines | get 2) }"),
        "must resume under the config directory from the contract"
    );
    assert!(
        stdout.contains("if $n < 2 or $n > 3 { return }"),
        "must require two or three lines"
    );
    assert!(
        stdout.contains("if $n == 3 {"),
        "must set the config dir only when the contract carried one"
    );
}

fn run_bash_wrapper_with_stub(stub_stdout: &str, stub_exit: i32) -> std::process::Output {
    run_shell_wrapper_with_stub("bash", stub_stdout, stub_exit)
}

fn run_shell_wrapper_with_stub(
    shell: &str,
    stub_stdout: &str,
    stub_exit: i32,
) -> std::process::Output {
    // Sources the generated bash wrapper, then calls `clauhist` with PATH overridden
    // so `command clauhist` resolves to a stub that emits `stub_stdout` and exits
    // with `stub_exit`. `cd` and `claude` are functions that echo their args (and,
    // for `claude`, the config dir it was given) so we can observe what the wrapper
    // invoked them with.
    let wrapper = run_init(shell);
    let stub_dir = unique_temp_path("bash-stub");
    std::fs::create_dir_all(&stub_dir).unwrap();
    let stub_path = stub_dir.join("clauhist");
    let stub = format!(
        "#!/usr/bin/env bash\nprintf '%s' {}\nexit {}\n",
        shell_quote(stub_stdout),
        stub_exit
    );
    std::fs::write(&stub_path, stub).unwrap();
    std::fs::set_permissions(
        &stub_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();

    let script = format!(
        "{wrapper}\n\
         cd() {{ echo CD:\"$@\"; }}\n\
         claude() {{ echo CLAUDE:\"${{CLAUDE_CONFIG_DIR}}:$@\"; }}\n\
         codex() {{ echo CODEX:\"${{CODEX_HOME}}:$@\"; }}\n\
         opencode() {{ echo OPENCODE:\"${{OPENCODE_DB}}:$@\"; }}\n\
         clauhist\n",
    );
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{path}", stub_dir.display());

    let out = std::process::Command::new(shell)
        .arg("-c")
        .arg(&script)
        .env("PATH", &path)
        // A CLAUDE_CONFIG_DIR in the test runner's own environment would
        // otherwise leak into the stub's output and mask a missing one.
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("OPENCODE_DB")
        .output()
        .unwrap();
    std::fs::remove_dir_all(stub_dir).unwrap();
    out
}

#[test]
fn bash_wrapper_runs_cd_and_claude_on_three_line_output() {
    let out = run_bash_wrapper_with_stub("/tmp/example\nabc-123\n/tmp/cfg\n", 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CD:-- /tmp/example"), "got: {stdout}");
    assert!(
        stdout.contains("CLAUDE:/tmp/cfg:--resume abc-123"),
        "must pass the config dir to claude; got: {stdout}"
    );
}

#[test]
fn bash_wrapper_resumes_without_the_env_var_on_two_line_output() {
    // Two lines mean the user never set CLAUDE_CONFIG_DIR. Resuming must then
    // leave it unset, so Claude Code keeps using its default ~/.claude.json
    // profile instead of starting logged out under ~/.claude/.claude.json.
    let out = run_bash_wrapper_with_stub("/tmp/example\nabc-123\n", 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("CD:-- /tmp/example"), "got: {stdout}");
    assert!(
        stdout.contains("CLAUDE::--resume abc-123"),
        "must resume with CLAUDE_CONFIG_DIR unset; got: {stdout}"
    );
}

#[test]
fn bash_wrapper_rejects_short_output() {
    // A single line is the canceled-fzf / malformed case. The wrapper must NOT
    // cd or run claude.
    let out = run_bash_wrapper_with_stub("only-one-line", 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("CD:"), "must not cd; got: {stdout}");
    assert!(
        !stdout.contains("CLAUDE:"),
        "must not resume; got: {stdout}"
    );
}

#[test]
fn bash_wrapper_rejects_empty_output() {
    let out = run_bash_wrapper_with_stub("", 0);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("CD:"));
    assert!(!stdout.contains("CLAUDE:"));
}

#[test]
fn bash_wrapper_rejects_failed_command() {
    // Non-zero exit (e.g. the project directory no longer exists) must not cd
    // or resume even if the stub printed a full contract.
    let out = run_bash_wrapper_with_stub("/tmp/example\nabc-123\n/tmp/cfg\n", 1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("CD:"),
        "must not cd on failure; got: {stdout}"
    );
    assert!(
        !stdout.contains("CLAUDE:"),
        "must not resume on failure; got: {stdout}"
    );
}

fn write_executable(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn write_codex_session(config: &std::path::Path, project: &str) {
    let dir = config.join("sessions/2026/10/30");
    std::fs::create_dir_all(&dir).unwrap();
    let lines = [
        serde_json::json!({"timestamp":"2026-10-30T01:00:00Z", "type":"session_meta",
            "payload":{"id":"codex-123", "cwd":project, "timestamp":"2026-10-30T01:00:00Z", "source":"cli"}}),
        serde_json::json!({"timestamp":"2026-10-30T01:01:00Z", "type":"event_msg",
            "payload":{"type":"user_message", "message":"日本語の Codex 履歴"}}),
        serde_json::json!({"timestamp":"2026-10-30T01:02:00Z", "type":"event_msg",
            "payload":{"type":"agent_message", "message":"done"}}),
    ];
    std::fs::write(
        dir.join("rollout-2026-10-30-codex-123.jsonl"),
        lines.iter().map(|v| format!("{v}\n")).collect::<String>(),
    )
    .unwrap();
}

#[test]
fn codex_preview_reads_rollouts_with_and_without_codex_home_override() {
    let root = unique_temp_path("codex-preview");
    let config = root.join(".codex");
    write_codex_session(&config, "/tmp/project with spaces");
    for home_override in [None, Some(""), Some(config.to_str().unwrap())] {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(["preview", "codex-123", "--source", "codex"])
            .env("HOME", &root);
        match home_override {
            Some(value) => cmd.env("CODEX_HOME", value),
            None => cmd.env_remove("CODEX_HOME"),
        };
        let output = cmd.output().unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Project : /tmp/project with spaces"),
            "{stdout}"
        );
        assert!(stdout.contains("日本語の Codex 履歴"), "{stdout}");
        assert!(stdout.contains("Messages: 1"), "{stdout}");
        assert!(
            !stdout.contains("done"),
            "preview shows user prompts like Claude's preview"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn browser_switches_from_empty_claude_history_to_codex_and_preserves_resume_target() {
    let root = unique_temp_path("switch");
    let config = root.join("codex config");
    let project = root.join(" ✓ project's directory");
    let bin = root.join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_codex_session(&config, project.to_str().unwrap());
    // Real fzf emits an expected key even if its empty selection exits with 1.
    write_executable(
        &bin.join("fzf"),
        &format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then exit 0; fi
printf '%s\n' "$@" >> {args}
if [ ! -f {state} ]; then
    cat > /dev/null
    touch {state}
    printf 'tab\n'
    exit 1
fi
printf '\n'
head -n 1
"#,
            args = shell_quote(root.join("args").to_str().unwrap()),
            state = shell_quote(root.join("state").to_str().unwrap())
        ),
    );
    let output = std::process::Command::new(BIN)
        .arg("--print")
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CLAUDE_CONFIG_DIR", root.join("no-claude-history"))
        .env("CODEX_HOME", &config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "clauhist:codex\n{}\ncodex-123\n{}\n",
            project.display(),
            config.display()
        )
    );
    let args = std::fs::read_to_string(root.join("args")).unwrap();
    assert!(args.contains("--expect=tab"));
    assert!(!args.contains("--expect=ctrl-s"));
    assert!(args.contains("Claude Code History"));
    assert!(args.contains("No history found for this source"));
    assert!(args.contains("Codex History"));
    assert!(args.contains("--source codex preview {1}"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bash_wrapper_resumes_codex_with_optional_config_and_quotes_values() {
    for config in [None, Some("/tmp/Codex's config $value")] {
        let contract = format!(
            "clauhist:codex\n/tmp/project's directory\ncodex-123{}\n",
            config.map(|cfg| format!("\n{cfg}")).unwrap_or_default()
        );
        let output = run_bash_wrapper_with_stub(&contract, 0);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("CD:-- /tmp/project's directory"),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("CODEX:{}:resume codex-123", config.unwrap_or(""))),
            "{stdout}"
        );
        assert!(!stdout.contains("CLAUDE:"), "{stdout}");
    }
}

#[test]
fn browser_resumes_codex_in_its_project_without_shell_integration() {
    let root = unique_temp_path("codex-resume");
    let config = root.join("Codex's config");
    let project = root.join("project with spaces");
    let bin = root.join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_codex_session(&config, project.to_str().unwrap());
    write_executable(
        &bin.join("fzf"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\nprintf '\\n'\nhead -n 1\n",
    );
    write_executable(
        &bin.join("codex"),
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$CODEX_HOME\" \"$@\" > \"$RESUME_LOG\"\n",
    );
    write_executable(&bin.join("test-shell"), "#!/bin/sh\nexit 0\n");
    let output = std::process::Command::new(BIN)
        .args(["--source", "codex"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CODEX_HOME", &config)
        .env("SHELL", bin.join("test-shell"))
        .env("RESUME_LOG", root.join("resumed"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("resumed")).unwrap(),
        format!(
            "{}\n{}\nresume\ncodex-123\n",
            project.display(),
            config.display()
        )
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Codex session ended."));
    std::fs::remove_dir_all(root).unwrap();
}

fn write_opencode_session(path: &std::path::Path, project: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, directory TEXT,
        parent_id TEXT, time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
        CREATE TABLE session_message (id TEXT, session_id TEXT, type TEXT,
        seq INTEGER, time_created INTEGER, data TEXT);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_v2 VALUES ('ses_test',?1,NULL,1000,3000,NULL)",
        [project.to_str().unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES ('msg_test','ses_test','user',1,2000,?1)",
        [r#"{"text":"OpenCode の履歴を確認"}"#],
    )
    .unwrap();
}

#[test]
fn opencode_preview_uses_xdg_and_absolute_or_relative_database_overrides() {
    let root = unique_temp_path("opencode-paths");
    let default_db = root.join("opencode/opencode.db");
    let custom_db = root.join("opencode/work.db");
    write_opencode_session(&default_db, &root);
    write_opencode_session(&custom_db, &root);
    for db_override in [
        None,
        Some(""),
        Some("work.db"),
        Some(custom_db.to_str().unwrap()),
    ] {
        let mut cmd = std::process::Command::new(BIN);
        cmd.args(["--source", "opencode", "preview", "ses_test"])
            .env("XDG_DATA_HOME", &root);
        match db_override {
            Some(value) => cmd.env("OPENCODE_DB", value),
            None => cmd.env_remove("OPENCODE_DB"),
        };
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("OpenCode の履歴を確認"), "{stdout}");
        assert!(
            stdout.contains(&format!("Project : {}", root.display())),
            "{stdout}"
        );
        assert!(stdout.contains("Messages: 1"), "{stdout}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn browser_cycles_through_all_three_sources_and_preserves_opencode_database() {
    let root = unique_temp_path("opencode-cycle");
    let bin = root.join("bin");
    let project = root.join("project's directory");
    let database = root.join("opencode/work.db");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_opencode_session(&database, &project);
    // OpenCode -> Claude -> Codex -> OpenCode. Empty sources must stay switchable.
    write_executable(
        &bin.join("fzf"),
        &format!(
            r#"#!/bin/sh
if [ "$1" = --version ]; then exit 0; fi
printf '%s\n' "$@" >> {args}
n=0
if [ -f {state} ]; then n=$(cat {state}); fi
n=$((n + 1))
printf '%s' "$n" > {state}
if [ "$n" -lt 4 ]; then
    cat > /dev/null
    printf 'tab\n'
    exit 1
fi
printf '\n'
head -n 1
"#,
            args = shell_quote(root.join("args").to_str().unwrap()),
            state = shell_quote(root.join("state").to_str().unwrap())
        ),
    );
    let output = std::process::Command::new(BIN)
        .args(["--source", "opencode", "--print"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("CLAUDE_CONFIG_DIR", root.join("no-claude"))
        .env("CODEX_HOME", root.join("no-codex"))
        .env("XDG_DATA_HOME", &root)
        .env("OPENCODE_DB", "work.db")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "clauhist:opencode\n{}\nses_test\n{}\n",
            project.display(),
            database.display()
        )
    );
    let args = std::fs::read_to_string(root.join("args")).unwrap();
    for header in ["OpenCode History", "Claude Code History", "Codex History"] {
        assert!(args.contains(header), "{args}");
    }
    assert_eq!(args.matches("--source opencode preview {1}").count(), 2);
    assert!(args.contains("--expect=tab"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shell_wrappers_resume_opencode_with_optional_database_override() {
    for shell in ["bash", "zsh"] {
        for config in [None, Some("/tmp/OpenCode's database $value.db")] {
            let contract = format!(
                "clauhist:opencode\n/tmp/project's directory\nses_test{}\n",
                config.map(|cfg| format!("\n{cfg}")).unwrap_or_default()
            );
            let output = run_shell_wrapper_with_stub(shell, &contract, 0);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                stdout.contains("CD:-- /tmp/project's directory"),
                "{stdout}"
            );
            assert!(
                stdout.contains(&format!(
                    "OPENCODE:{}:--session ses_test",
                    config.unwrap_or("")
                )),
                "{stdout}"
            );
            assert!(
                !stdout.contains("CLAUDE:") && !stdout.contains("CODEX:"),
                "{stdout}"
            );
        }
    }
}

#[test]
fn opencode_reader_sees_live_wal_and_does_not_modify_database() {
    let root = unique_temp_path("opencode-wal");
    let database = root.join("opencode.db");
    write_opencode_session(&database, &root);
    let writer = rusqlite::Connection::open(&database).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .execute(
            "UPDATE session_message SET data=?1",
            [r#"{"text":"committed WAL prompt"}"#],
        )
        .unwrap();
    let before = std::fs::read(&database).unwrap();
    let wal_before = std::fs::read(root.join("opencode.db-wal")).unwrap();
    let output = std::process::Command::new(BIN)
        .args(["--source", "opencode", "preview", "ses_test"])
        .env("OPENCODE_DB", &database)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("committed WAL prompt"));
    assert_eq!(std::fs::read(&database).unwrap(), before);
    assert_eq!(
        std::fs::read(root.join("opencode.db-wal")).unwrap(),
        wal_before
    );
    drop(writer);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn browser_resumes_opencode_without_shell_integration() {
    let root = unique_temp_path("opencode-resume");
    let database = root.join("OpenCode's database.db");
    let project = root.join("project with spaces");
    let bin = root.join("bin");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_opencode_session(&database, &project);
    write_executable(
        &bin.join("fzf"),
        "#!/bin/sh\nif [ \"$1\" = --version ]; then exit 0; fi\nprintf '\\n'\nhead -n 1\n",
    );
    write_executable(
        &bin.join("opencode"),
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$OPENCODE_DB\" \"$@\" > \"$RESUME_LOG\"\n",
    );
    write_executable(&bin.join("test-shell"), "#!/bin/sh\nexit 0\n");
    let output = std::process::Command::new(BIN)
        .args(["--source", "opencode"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("OPENCODE_DB", &database)
        .env("SHELL", bin.join("test-shell"))
        .env("RESUME_LOG", root.join("resumed"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("resumed")).unwrap(),
        format!(
            "{}\n{}\n--session\nses_test\n",
            project.display(),
            database.display()
        )
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("OpenCode session ended."));
    std::fs::remove_dir_all(root).unwrap();
}

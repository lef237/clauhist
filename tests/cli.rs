//! End-to-end tests that drive the compiled `clauhist` binary.
//!
//! These live in `tests/` on purpose: Cargo builds the binary before running
//! integration tests and exposes its path through `CARGO_BIN_EXE_clauhist`, so
//! they always exercise the current code. Keeping them in the unit-test module
//! instead meant reaching into `target/debug/clauhist`, which `cargo test` does
//! not rebuild — tests could silently run against a stale binary (or fail when
//! it had never been built).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_clauhist");

fn home_path(relative: &str) -> String {
    let home = std::env::var("HOME").unwrap();
    format!("{home}/{relative}")
}

fn unique_temp_path(label: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let home = std::env::var("HOME").unwrap();
    PathBuf::from(home)
        .join("tmp")
        .join(format!("clauhist-{label}-{suffix}"))
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
        stdout.contains(r#"if [[ -n "$cfg" ]]; then"#),
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
    assert!(stdout.contains(r#"if [[ -n "$cfg" ]]; then"#));
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
    // Sources the generated bash wrapper, then calls `clauhist` with PATH overridden
    // so `command clauhist` resolves to a stub that emits `stub_stdout` and exits
    // with `stub_exit`. `cd` and `claude` are functions that echo their args (and,
    // for `claude`, the config dir it was given) so we can observe what the wrapper
    // invoked them with.
    let wrapper = run_init("bash");
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
         clauhist\n",
    );
    let mut path = std::env::var("PATH").unwrap_or_default();
    path = format!("{}:{path}", stub_dir.display());

    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("PATH", &path)
        // A CLAUDE_CONFIG_DIR in the test runner's own environment would
        // otherwise leak into the stub's output and mask a missing one.
        .env_remove("CLAUDE_CONFIG_DIR")
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

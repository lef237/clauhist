# clauhist

Browse and resume Claude Code chat sessions interactively.

```
╭─────────────────────────────────────────────────────────────────────────────────╮
│ Claude Code History Browser  [Enter: resume  Ctrl-/: toggle preview  Ctrl-C: cancel] │
├─────────────────────────────────────────────────────────────────────────────────┤
│ Search:                                                                         │
│ > 2026-03-18 09:12  ✓ ~/projects/myapp      Tell me about Rust error handling…  (12) │
│   2026-03-17 22:45  ✓ ~/sandbox/api-client  Generate client from OpenAPI schema  (8) │
│   2026-03-17 14:30  ✗ ~/old-project         Database migration steps             (3) │
╰─────────────────────────────────────────────────────────────────────────────────╯
```
*(Example output — actual appearance depends on your terminal and fzf version)*

Select a session and press `Enter` — clauhist opens `claude --resume` in the project directory. When you exit Claude, you return to your original shell.

---

## Requirements

| Dependency | Required | Install |
|------------|----------|---------|
| [Rust](https://rustup.rs/) | Build only | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| [fzf](https://github.com/junegunn/fzf) | **Runtime** | `brew install fzf` |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | **Runtime** | `npm install -g @anthropic-ai/claude-code` |

> **Note:** fzf provides the interactive UI — clauhist will not work without it.

---

## Installation

```sh
git clone <repo-url>
cd clauhist
cargo install --path .
```

This installs the `clauhist` binary to `~/.cargo/bin/`. Make sure that directory is in your `PATH`:

```sh
# Verify
clauhist --version

# If ~/.cargo/bin is not in your PATH, add this to ~/.zshrc or ~/.bashrc:
export PATH="$HOME/.cargo/bin:$PATH"
```

---

## Usage

### Open the session browser

```sh
clauhist
```

The fzf browser opens with your Claude Code sessions sorted by most recent activity. Select a session and press `Enter` to resume it.

### Key bindings

| Key        | Action                            |
|------------|-----------------------------------|
| `Enter`    | Resume the selected session       |
| Type       | Filter sessions by keyword        |
| `↑` / `↓` | Move up / down                    |
| `Ctrl-/`   | Toggle the preview pane           |
| `Ctrl-C`   | Cancel and exit                   |

### Reading the list

```
2026-03-18 09:12  ✓ ~/projects/myapp  First message preview…  (12)
│                 │                                             └── message count
│                 └── ✓ project directory exists
│                     ✗ directory not found (deleted or moved)
└── last activity timestamp
```

The preview pane (toggle with `Ctrl-/`) shows the project path, timestamps, and all messages in the session.

---

## Shell integration (recommended)

By default, clauhist runs `cd` in a subshell, so your shell stays in the original directory after Claude exits. To stay in the project directory and enable `cd -` to go back, add shell integration:

```sh
# ~/.zshrc or ~/.bashrc
eval "$(clauhist init zsh)"   # or bash, fish
```

With this, selecting a session changes your current shell's directory and resumes Claude. After Claude exits, you remain in the project directory, and `cd -` takes you back.

---

## Troubleshooting

**`fzf not found`**
Install fzf: `brew install fzf` (macOS) or see the [fzf installation guide](https://github.com/junegunn/fzf#installation).

**`History file not found`**
`~/.claude/history.jsonl` does not exist yet. Start a chat in Claude Code to create it.

**`clauhist: command not found`**
`~/.cargo/bin` is not in your `PATH`. Add `export PATH="$HOME/.cargo/bin:$PATH"` to `.zshrc`.

**Sessions marked with `✗`**
The project directory has been deleted or moved. The session can still be resumed, but the `cd` step will fail. Claude will open in the directory where you ran `clauhist`.

---

## Information

clauhist is a local-only tool that works entirely on your machine.

- **What it reads:** `~/.claude/history.jsonl` — a local file that Claude Code stores on your machine. This file contains session metadata (session IDs, timestamps, project paths, and the first line of each user message).
- **What it does NOT do:** clauhist does not access Anthropic's API or servers, and does not transmit any data externally.
- **How it resumes sessions:** clauhist invokes `claude --resume <session-id>`, which is an [officially documented CLI command](https://docs.anthropic.com/en/docs/claude-code/cli-reference).

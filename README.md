# clauhist

Browse Claude Code, Codex, and OpenCode history across working directories and resume sessions.

```
╭───────────────────────────────────────────────────────────────────────────────────────╮
│ Claude Code History  [Tab: switch to Codex  Enter: resume  Ctrl-O: preview]          │
├───────────────────────────────────────────────────────────────────────────────────────┤
│ Search:                                                                               │
│ > 2026-03-18 09:12  ✓ ~/projects/myapp      Tell me about Rust error handling…  (12)  │
│   2026-03-17 22:45  ✓ ~/sandbox/api-client  Generate client from OpenAPI schema  (8)  │
│   2026-03-17 14:30  ✗ ~/old-project         Database migration steps             (3)  │
╰───────────────────────────────────────────────────────────────────────────────────────╯
```
*(Example output — actual appearance depends on your terminal and fzf version)*

Claude Code can already resume work from the current project directory. clauhist solves a different problem: browsing the full history across all of your working directories from one place.

Press `Tab` to cycle through the Claude Code, Codex, and OpenCode screens. Select a session and press `Enter` — clauhist opens `claude --resume`, `codex resume`, or `opencode --session` in the project directory.

---

## Why clauhist exists

If you `cd` into a project and use Claude Code's `/resume` there, you can inspect and reopen work for that directory.

The limitation is that this is tied to the directory you are currently in. If you want to look back across many repositories or old working directories, you have to move around and check them one by one.

clauhist reads `~/.claude/history.jsonl` — or `$CLAUDE_CONFIG_DIR/history.jsonl` if you have moved Claude Code's config directory — shows sessions from all working directories in one `fzf` list, and lets you reopen the one you want immediately.

For Codex, clauhist reads local session rollouts under `~/.codex/sessions/` (or `$CODEX_HOME/sessions/`). These contain the session's working directory, timestamps, and user prompts. Internal agent sessions and archived sessions are excluded.

---

## Requirements

| Dependency | Required |
|------------|----------|
| [Rust](https://rustup.rs/) | Build only |
| [fzf](https://github.com/junegunn/fzf) | **Runtime** |
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) / [Codex CLI](https://developers.openai.com/codex/cli/) / [OpenCode](https://opencode.ai/docs/cli/) | **Runtime**, for the sessions you resume |

> **Note:** fzf provides the interactive UI — clauhist will not work without it.

---

## Installation

```sh
cargo install clauhist
```

Or build from source:

```sh
git clone https://github.com/lef237/clauhist.git
cd clauhist
cargo install --path .
```

Make sure `~/.cargo/bin` is in your `PATH`:

```sh
# If ~/.cargo/bin is not in your PATH, add this to ~/.zshrc or ~/.bashrc:
export PATH="$HOME/.cargo/bin:$PATH"
```

---

## Usage

### Open the session browser

```sh
clauhist
```

The fzf browser opens with your Claude Code sessions sorted by most recent activity, across all working directories found in your history. Press `Tab` to cycle through Claude Code → Codex → OpenCode → Claude Code. The active source and switching key are shown in the header. You can switch even when one source has no history.

To open Codex or OpenCode directly:

```sh
clauhist --source codex
clauhist --source opencode
```

Use `clauhist --source claude` to open Claude Code explicitly. Select a session and press `Enter` to resume it with the corresponding CLI.

Resuming hands the terminal to Claude Code, which replays the whole session transcript before it can draw anything — several seconds for a long session. clauhist prints the session it is handing over as it does so, so the blank terminal in between does not look like a hang.

### Key bindings

| Key        | Action                            |
|------------|-----------------------------------|
| `Enter`    | Resume the selected session       |
| `Tab`      | Cycle through Claude Code, Codex, and OpenCode |
| Type       | Filter sessions by keyword        |
| `↑` / `↓` | Move up / down                    |
| `Ctrl-O`   | Toggle the preview pane           |
| `Ctrl-/`   | Toggle the preview pane (not supported by every terminal) |
| `Ctrl-X`   | Expand the preview / restore the normal half-height layout |
| `Ctrl-C`   | Cancel and exit                   |

### Reading the list

```
2026-03-18 09:12  ✓ ~/projects/myapp  First message preview…  (12)
│                 │                                             └── message count
│                 └── ✓ project directory exists
│                     ✗ directory not found (deleted or moved)
└── last activity timestamp
```

The preview pane (toggle with `Ctrl-O`) shows the project path, timestamps, and user prompts in the session. The count is the number of user prompts, for all three sources.

Its top border includes a resize handle and a keyboard hint:

```text
────── ↕ Drag to resize · Ctrl-X: expand/restore ──────
```

Drag this border up or down to adjust the split. Press `Ctrl-X` to give the preview the largest space fzf allows, keeping a small area for the session list. Press `Ctrl-X` again to return to the normal 50/50 split. This also opens the preview if it is hidden.

---

## Multiple accounts

Claude Code supports more than one account on a machine through the `CLAUDE_CONFIG_DIR` environment variable — for example a personal seat in `~/.claude` and a team seat in `~/.claude-work`. clauhist reads history from the same location, and with shell integration installed it resumes the session under that account as well. To browse and resume the team seat:

```sh
# zsh / bash
CLAUDE_CONFIG_DIR="$HOME/.claude-work" clauhist
```

When the variable is unset or empty, clauhist reads `~/.claude` as before, and resumes without setting `CLAUDE_CONFIG_DIR` at all — Claude Code then stays on the default profile it already uses, so you remain logged in.

A small function keeps it short:

```sh
# ~/.zshrc or ~/.bashrc
clauhist-work() { CLAUDE_CONFIG_DIR="$HOME/.claude-work" clauhist "$@"; }
```

```fish
# ~/.config/fish/config.fish
function clauhist-work
    set -lx CLAUDE_CONFIG_DIR $HOME/.claude-work
    clauhist $argv
end
```

---

## Codex home directory

By default, Codex history is read from `~/.codex/sessions/`. If you use a separate Codex home, set `CODEX_HOME`; the selected session is resumed with the same override:

```sh
CODEX_HOME="$HOME/.codex-work" clauhist --source codex
```

When `CODEX_HOME` is unset or empty, clauhist uses `~/.codex` and does not add an override to the resume command. Only locally saved sessions are available; remote-only conversations are not included.

---

## OpenCode database

OpenCode history is read directly from its local SQLite database, without starting an OpenCode server. Both the V1 (`session` / `message` / `part`) and V2 (`session_v2` / `session_message`) schemas are supported. If both exist, clauhist uses V2. Child-agent, archived, and remote-only sessions without a local directory are excluded.

The default database is `~/.local/share/opencode/opencode.db`. Set `XDG_DATA_HOME` to use a different data directory, or `OPENCODE_DB` to select a database:

```sh
OPENCODE_DB="$HOME/work-history.db" clauhist --source opencode
```

Relative `OPENCODE_DB` paths are resolved under the OpenCode data directory, matching OpenCode. The resolved override is also passed when resuming a session. For development channels or a custom database filename, set `OPENCODE_DB` explicitly. SQLite is bundled into clauhist; no separate `sqlite3` command is required. Older OpenCode installations using JSON-file storage must migrate to SQLite first.

---

## Shell integration (recommended)

By default, clauhist resumes the session in a sub-shell running your `$SHELL` (falling back to zsh). Therefore, you need to `exit` — or run `clauhist --return` — to get back to the original directory. Both leave the sub-shell the normal way, so its shell history is written out; `clauhist --return` only works when you run it directly in the sub-shell clauhist started.

To stay in the current shell and enable `cd -` to go back, add shell integration:

```sh
# ~/.zshrc or ~/.bashrc
eval "$(clauhist init zsh)"   # or bash, fish
```

For [Nushell](https://www.nushell.sh/), add to your `config.nu`:

```nu
clauhist init nu | save -f ($nu.default-config-dir | path join "clauhist.nu")
source ($nu.default-config-dir | path join "clauhist.nu")
```

The wrapper is regenerated on every Nushell start, so it always matches the
installed `clauhist` — no separate step is needed after upgrading.

With this, selecting a session changes your current shell's directory and resumes Claude Code, Codex, or OpenCode. After it exits, you remain in the project directory, and `cd -` takes you back.

After upgrading to a version with OpenCode support, reload your shell integration (`eval "$(clauhist init zsh)"`, or the command for your shell). Existing wrappers must be regenerated to recognize OpenCode selections. If you pasted the generated function into your shell configuration, replace it with the new `clauhist init <shell>` output.

---

## Troubleshooting

**`fzf not found`**
Install fzf: `brew install fzf` (macOS) or see the [fzf installation guide](https://github.com/junegunn/fzf#installation).

**`No history found for this source`**
Press `Tab` to check the other tools. For Claude Code, check `~/.claude/history.jsonl` (or `$CLAUDE_CONFIG_DIR/history.jsonl`). For Codex, check `~/.codex/sessions/` (or `$CODEX_HOME/sessions/`). For OpenCode, check `~/.local/share/opencode/opencode.db`, `$XDG_DATA_HOME/opencode/opencode.db`, or your `OPENCODE_DB` override. Start a local session in the corresponding tool to create history.

**`clauhist: command not found`**
`~/.cargo/bin` is not in your `PATH`. Add `export PATH="$HOME/.cargo/bin:$PATH"` to `.zshrc`.

**`Ctrl-/` does not toggle the preview**
fzf treats `Ctrl-/` as an alias for `Ctrl-_` (ASCII `0x1F`), and some terminals — WezTerm, for example — never emit that byte. Use `Ctrl-O` instead; it is a plain ASCII control character and works in every terminal.

**Claude Code asks you to log in after resuming**
Fixed in clauhist 1.4.0. Earlier versions always passed `CLAUDE_CONFIG_DIR` to `claude`, even when you had never set it. Claude Code reads its account from `~/.claude.json` when the variable is unset but from `$CLAUDE_CONFIG_DIR/.claude.json` when it is set, so resuming started an empty, logged-out profile. Upgrade with `cargo install clauhist`, then re-run `clauhist init <shell>` output if you paste it into your rc file literally. The stray profile it created can be removed: `rm ~/.claude/.claude.json` (only if you never set `CLAUDE_CONFIG_DIR=~/.claude` yourself).

**Sessions marked with `✗`**
The project directory has been deleted or moved. clauhist resumes a session by `cd`-ing into its project directory first, so these sessions cannot be resumed — selecting one reports the missing directory and exits. Restore or recreate the directory at its original path to resume the session.

---

## Information

clauhist is a local-only tool that works entirely on your machine.

- **What it reads:** Claude Code's `history.jsonl`, Codex's `sessions/**/*.jsonl`, and OpenCode's SQLite session/message tables under their respective data directories. Codex rollouts contain transcripts; clauhist displays their session metadata and user prompts.
- **What it does NOT do:** clauhist does not call Anthropic or OpenAI APIs, and does not transmit history externally. Resuming starts the corresponding CLI, which uses its normal connection and settings.
- **How it resumes sessions:** clauhist invokes `claude --resume <session-id>`, `codex resume <session-id>`, or `opencode --session <session-id>`. See the official [Claude Code CLI reference](https://docs.anthropic.com/en/docs/claude-code/cli-reference), [Codex CLI reference](https://developers.openai.com/codex/cli/reference/#codex-resume), and [OpenCode CLI reference](https://opencode.ai/docs/cli/).

---

## Releasing

Maintainers: see [RELEASING.md](RELEASING.md). A release must update **both**
crates.io (`cargo publish`) and the GitHub Releases page (`gh release create`) —
they are independent steps.

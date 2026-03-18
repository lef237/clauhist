#!/usr/bin/env python3
"""
clauhist - Claude Code チャット履歴ブラウザ

使い方:
  clauhist               # インタラクティブブラウザを起動
  clauhist preview <sessionId>  # プレビュー表示（fzf 内部使用）
"""
import json
import subprocess
import sys
import shlex
from pathlib import Path
from datetime import datetime
from collections import defaultdict

HISTORY_FILE = Path.home() / ".claude" / "history.jsonl"


def read_sessions():
    sessions = defaultdict(list)
    try:
        with open(HISTORY_FILE, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    entry = json.loads(line)
                    sid = entry.get("sessionId")
                    if sid:
                        sessions[sid].append(entry)
                except json.JSONDecodeError:
                    continue
    except FileNotFoundError:
        pass
    return sessions


def truncate(text, max_len):
    return text[:max_len] + "…" if len(text) > max_len else text


def format_for_fzf(sessions):
    items = []
    for sid, entries in sessions.items():
        entries.sort(key=lambda x: x.get("timestamp", 0))
        first = entries[0]
        project = first.get("project", "unknown")
        ts = first.get("timestamp", 0) / 1000
        date_str = datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M")
        first_msg = first.get("display", "").replace("\n", " ").strip()
        first_msg = truncate(first_msg, 70)
        msg_count = len(entries)
        exists = "✓" if Path(project).exists() else "✗"
        # タブ区切り: sessionId | date | exists+project | first_msg | count
        line = f"{sid}\t{date_str}\t{exists} {project}\t{first_msg}\t({msg_count})"
        items.append((ts, line))

    items.sort(key=lambda x: x[0], reverse=True)
    return [item[1] for item in items]


def preview_session(sid):
    sessions = read_sessions()
    entries = sessions.get(sid, [])
    if not entries:
        print(f"Session not found: {sid}")
        return

    entries.sort(key=lambda x: x.get("timestamp", 0))
    project = entries[0].get("project", "")
    first_ts = entries[0].get("timestamp", 0) / 1000
    last_ts = entries[-1].get("timestamp", 0) / 1000

    print(f"Project : {project}")
    print(f"Session : {sid}")
    print(f"Started : {datetime.fromtimestamp(first_ts).strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"Last    : {datetime.fromtimestamp(last_ts).strftime('%Y-%m-%d %H:%M:%S')}")
    print(f"Messages: {len(entries)}")
    print("─" * 60)

    for entry in entries:
        ts = entry.get("timestamp", 0) / 1000
        time_str = datetime.fromtimestamp(ts).strftime("%H:%M")
        msg = entry.get("display", "").replace("\n", " ").strip()
        if msg:
            print(f"[{time_str}] {truncate(msg, 120)}")


def interactive_browser():
    if not HISTORY_FILE.exists():
        print(f"履歴ファイルが見つかりません: {HISTORY_FILE}", file=sys.stderr)
        sys.exit(1)

    sessions = read_sessions()
    if not sessions:
        print("履歴がありません", file=sys.stderr)
        sys.exit(1)

    lines = format_for_fzf(sessions)
    fzf_input = "\n".join(lines)
    script_path = Path(__file__).resolve()

    fzf_cmd = [
        "fzf",
        "--delimiter=\t",
        "--with-nth=2,3,4,5",  # date, ✓/✗+project, first_msg, count
        f"--preview={sys.executable} {script_path} preview {{1}}",
        "--preview-window=down:40%:wrap",
        "--height=85%",
        "--border=rounded",
        "--header=Claude Code 履歴ブラウザ  [Enter: 再開  Ctrl-C: キャンセル]",
        "--prompt=検索: ",
        "--no-sort",
        "--tiebreak=index",
        "--bind=ctrl-/:toggle-preview",
    ]

    result = subprocess.run(fzf_cmd, input=fzf_input, capture_output=True, text=True)

    if result.returncode != 0 or not result.stdout.strip():
        sys.exit(0)

    selected = result.stdout.strip()
    fields = selected.split("\t")
    if len(fields) < 3:
        sys.exit(1)

    sid = fields[0]
    # fields[2] は "✓ /path" or "✗ /path"
    project = fields[2][2:].strip()

    # シェルコマンドを stdout に出力（呼び出し元の shell 関数が eval する）
    print(f"cd {shlex.quote(project)} && claude --resume {sid}")


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "preview":
        preview_session(sys.argv[2])
    else:
        interactive_browser()

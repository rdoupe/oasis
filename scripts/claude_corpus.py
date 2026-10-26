#!/usr/bin/env python3
"""Turn real Claude Code sessions into anonymized corpus cases.

The corpus under `tests/fixtures/claude-corpus/` checks what Oasis reads from
Claude Code: goals and loops from the session transcript, unfinished subagents
from their records beside it, and background work from the screen. After a
Claude Code upgrade, capture a few cases from live sessions and run
`just claude-corpus`.

Every word of the session's own text (prompts, replies, tool input and output,
paths, names) is replaced by a made-up word of the same length and case, so the
excerpt keeps its real shape. Each occurrence gets its own random word, so
word frequencies give nothing away. The words Claude Code itself draws or
writes, the ids that link records, and the fields Oasis reads are kept. Text
longer than 200 characters is cut there: what Oasis reads starts a value.

    # A transcript excerpt (lines are 1-based and inclusive):
    scripts/claude_corpus.py transcript SESSION.jsonl --lines 120:180 --case my-case
    # The same session's subagent records:
    scripts/claude_corpus.py subagents SESSION.jsonl --case my-case
    # A screen, as `oasis agent read <pane> --source detection --format text` prints it:
    scripts/claude_corpus.py screen SCREEN.txt --case my-case

Each command writes a `case.toml` skeleton when the case has none; fill in what
Oasis should read, checked against the source, not against Oasis.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import sys
from pathlib import Path

CORPUS = Path(__file__).resolve().parent.parent / "tests" / "fixtures" / "claude-corpus"

# Words Claude Code draws or writes itself, which the readers or a reviewer
# need: footer and notice text, goal and loop records, units.
KEEP_WORDS = {
    word.lower()
    for word in """
    goal goals active achieved cleared after an unrecoverable error authentication failed
    turn turns tokens token command name loop scheduled recurring job every minute minutes
    hour hours next run shell shells monitor monitors mcp task tasks agent agents main
    general purpose explore plan bypass permissions on off shift tab to cycle accept edits
    auto mode for shortcuts done am pm update available mise upgrade claude opus sonnet
    haiku fable effort low medium high xhigh max context read cached uncached write esc
    interrupt worked crunched cooked baked thinking background running stopped
    email tailnet ip could not be new clear save
    """.split()
}

# Values kept as they are: record structure, ids, times and versions.
KEEP_VALUE_KEYS = {
    "type", "subtype", "role", "stop_reason", "model", "version", "userType",
    "entrypoint", "taskKind", "operation", "level", "timestamp", "uuid",
    "parentUuid", "logicalParentUuid", "promptId", "requestId", "id",
    "tool_use_id", "toolUseID", "sourceToolAssistantUUID", "agentId",
    "permissionMode", "requestShape", "cron", "kind", "promptSource", "turnOrigin",
    "trigger", "source", "producer", "scheduledTaskId",
}
# Claude Code's own subagent types; a custom one is named by its user.
BUILTIN_AGENTS = {"general-purpose", "Explore", "Plan", "statusline-setup", "claude-code-guide", "claude"}
# Records whose `name` is the tool called, which Oasis reads.
TOOL_CALLS = {"tool_use", "server_tool_use"}
HEX_ID = re.compile(r"(?=[0-9a-f]*[0-9])[0-9a-f]{6,}")
IDENTIFIER = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
TAILNET_IP = re.compile(r"\b100\.\d{1,3}\.\d{1,3}\.\d{1,3}\b")
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")


LONG_TEXT = 200


class Scrambler:
    def __init__(self) -> None:
        self.keys: dict[str, str] = {}

    def word(self, word: str) -> str:
        if len(word) <= 1 or word.lower() in KEEP_WORDS or HEX_ID.fullmatch(word):
            return word
        return "".join(
            (new.upper() if old.isupper() else new)
            for old, new in zip(word, (secrets.choice("abcdefghijklmnopqrstuvwxyz") for _ in word))
        )

    def text(self, text: str) -> str:
        text = TAILNET_IP.sub("<tailnet-ip>", EMAIL.sub("<email>", text))
        # Random ids (sessions, records) link records and say nothing personal.
        pieces = UUID.split(text)
        ids = UUID.findall(text)
        out = [re.sub(r"[A-Za-z0-9]+", lambda match: self._token(match.group()), piece) for piece in pieces]
        return "".join(part for pair in zip(out, ids + [""]) for part in pair)

    def _token(self, token: str) -> str:
        if HEX_ID.fullmatch(token):
            return token
        return re.sub(r"[A-Za-z]+", lambda match: self.word(match.group()), token)

    def key_name(self, key: str) -> str:
        if IDENTIFIER.fullmatch(key):
            return key
        return self.keys.setdefault(key, f"key{len(self.keys) + 1}")

    def value(self, value, key: str | None = None):
        if isinstance(value, dict):
            tool_call = isinstance(value.get("type"), str) and value["type"] in TOOL_CALLS
            return {
                self.key_name(k): v if k == "name" and tool_call and isinstance(v, str) else self.value(v, k)
                for k, v in value.items()
            }
        if isinstance(value, list):
            return [self.value(item, key) for item in value]
        if isinstance(value, str):
            if key in KEEP_VALUE_KEYS:
                return value
            if key == "agentType" and value in BUILTIN_AGENTS:
                return value
            if len(value) > LONG_TEXT:
                value = value[:LONG_TEXT] + "…"
            return self.text(value)
        return value


def transcript_lines(path: Path) -> list[str]:
    return path.read_text(errors="replace").splitlines()


def case_dir(name: str) -> Path:
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]*", name):
        sys.exit(f"case names are lowercase words joined by '-': {name!r}")
    path = CORPUS / name
    path.mkdir(parents=True, exist_ok=True)
    return path


def claude_version(lines: list[str]) -> str:
    for line in reversed(lines):
        match = re.search(r'"version":\s*"(\d+\.\d+\.\d+)"', line)
        if match:
            return match.group(1)
    return "unknown"


def write_skeleton(case: Path, version: str) -> None:
    toml = case / "case.toml"
    if toml.exists():
        return
    toml.write_text(
        f'claude = "{version}"\n'
        'about = "TODO: what happened in the session, in plain words"\n'
        "\n"
        "# Transcript reads, in order, each after the line it names (1-based;\n"
        "# omit `after_line` for the whole excerpt).\n"
        "# goal: unknown | active | achieved | none\n"
        "# loop: none | running | wakeup | every <interval>, e.g. every 5m\n"
        "[[read]]\n"
        'goal = "TODO"\n'
        'loop = "TODO"\n'
    )


def cmd_transcript(args: argparse.Namespace) -> None:
    lines = transcript_lines(args.session)
    start, _, end = args.lines.partition(":")
    first, last = int(start), int(end or len(lines))
    if not 1 <= first <= last <= len(lines):
        sys.exit(f"--lines must fall within 1:{len(lines)}")
    scrambler = Scrambler()
    out = []
    for line in lines[first - 1 : last]:
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            out.append(scrambler.text(line))  # a line cut off mid-write
            continue
        if not args.keep_requests and str(entry.get("type", "")).startswith("api-request"):
            # A copy of the whole request; a placeholder keeps line numbers.
            out.append(json.dumps({"type": entry["type"], "omitted": "request copy"}, separators=(",", ":")))
            continue
        out.append(json.dumps(scrambler.value(entry), ensure_ascii=False, separators=(",", ":")))
    case = case_dir(args.case)
    (case / "transcript.jsonl").write_text("\n".join(out) + "\n")
    write_skeleton(case, claude_version(lines[first - 1 : last]))
    print(f"{case / 'transcript.jsonl'}: {len(out)} lines")


def cmd_subagents(args: argparse.Namespace) -> None:
    source = args.session.with_suffix("") / "subagents"
    # Each subagent has a transcript (agent-<id>.jsonl) and, once Claude
    # writes it, a record (agent-<id>.meta.json). Records are kept;
    # transcripts become empty placeholders, since only their presence counts.
    ids = sorted(
        {path.name.removesuffix(".meta.json") for path in source.glob("*.meta.json")}
        | {path.name.removesuffix(".jsonl") for path in source.glob("*.jsonl")}
    )
    case = case_dir(args.case)
    # Beside the transcript, as Claude keeps them.
    target = case / "transcript" / "subagents"
    target.mkdir(parents=True, exist_ok=True)
    scrambler = Scrambler()
    for index, agent in enumerate(ids, 1):
        meta = source / f"{agent}.meta.json"
        if meta.exists():
            data = scrambler.value(json.loads(meta.read_text()))
            (target / f"agent-{index}.meta.json").write_text(
                json.dumps(data, ensure_ascii=False, separators=(",", ":")) + "\n"
            )
        if (source / f"{agent}.jsonl").exists():
            (target / f"agent-{index}.jsonl").write_text("")
    print(f"{target}: {len(ids)} subagents")


def cmd_screen(args: argparse.Namespace) -> None:
    text = args.screen.read_text(errors="replace")
    case = case_dir(args.case)
    (case / "screen.txt").write_text(Scrambler().text(text))
    write_skeleton(case, args.claude or "unknown")
    print(case / "screen.txt")


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    transcript = commands.add_parser("transcript", help="add an anonymized transcript excerpt")
    transcript.add_argument("session", type=Path)
    transcript.add_argument("--lines", required=True, help="FIRST:LAST, 1-based and inclusive")
    transcript.add_argument("--case", required=True)
    transcript.add_argument(
        "--keep-requests",
        action="store_true",
        help="keep api-request records, which copy the whole system prompt",
    )
    transcript.set_defaults(run=cmd_transcript)

    subagents = commands.add_parser("subagents", help="add the session's subagent records")
    subagents.add_argument("session", type=Path)
    subagents.add_argument("--case", required=True)
    subagents.set_defaults(run=cmd_subagents)

    screen = commands.add_parser("screen", help="add an anonymized screen capture")
    screen.add_argument("screen", type=Path)
    screen.add_argument("--case", required=True)
    screen.add_argument("--claude", help="the Claude Code version the screen came from")
    screen.set_defaults(run=cmd_screen)

    args = parser.parse_args(argv)
    args.run(args)


if __name__ == "__main__":
    main()

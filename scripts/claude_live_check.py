#!/usr/bin/env python3
"""Check that Oasis reads a newly installed Claude Code correctly.

Runs the given Claude Code version in a private Oasis server (session
`claude-check`, never the user's own) through short live scenarios: a goal
met, a goal it gives up on, a typed prompt after each, a background shell, a
background subagent and a repeating /loop. After each, what Oasis shows is
compared with what Claude itself recorded in its transcript. Goal records and
screen lines Oasis does not know yet fail the check too: that is how a new
Claude outcome first shows up.

Everything lands in ~/.cache/oasis-claude-check/<version>/: the report, the
server log, the session transcript and the screens at each step, ready for
`scripts/claude_corpus.py`. When the check fails and `--report-to-oasis` is
given, a tab opens in the user's Oasis with Claude briefed to fix Oasis.

    scripts/claude_live_check.py                 # the latest installed Claude
    scripts/claude_live_check.py --claude 2.1.311 --report-to-oasis

It spends a few cents of Haiku tokens per run.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CACHE = Path.home() / ".cache" / "oasis-claude-check"
SESSION = "claude-check"
AGENT = "check"
MODEL = "haiku"

# What Oasis knows Claude writes; anything else in these places is new.
KNOWN_GOAL_KEYS = {
    "type", "met", "sentinel", "failed", "condition", "reason", "iterations",
    "durationMs", "tokens",
}
KNOWN_GOAL_SCREEN = ("✔ Goal achieved", "✘ Goal could not be achieved", "◎ /goal active")
# Claude's own goal lines start with a glyph and `Goal` or `/goal`; its
# replies (`● …`) are prose.
GOAL_GLYPHS = ("✔", "✘", "✗", "◎", "⚠", "✓", "✕")
KNOWN_GOAL_NOTICES = ("Goal cleared", "Goal paused")


def oasis_env() -> dict[str, str]:
    # Never let an inherited socket point the check at the user's server.
    env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
    env.pop("TMUX", None)
    return env


class Oasis:
    def __init__(self, binary: Path, session: str | None) -> None:
        self.binary = binary
        self.session = session

    def run(self, *args: str, timeout: float = 60, check: bool = True) -> dict:
        command = [str(self.binary)]
        if self.session:
            command += ["--session", self.session]
        result = subprocess.run(
            command + list(args), capture_output=True, text=True, env=oasis_env(), timeout=timeout
        )
        try:
            data = json.loads(result.stdout) if result.stdout.strip() else {}
        except json.JSONDecodeError:
            data = {"raw": result.stdout}
        if check and (result.returncode != 0 or "error" in data):
            raise RuntimeError(f"oasis {' '.join(args)}: {result.stdout or result.stderr}".strip())
        return data

    def pane(self, pane_id: str) -> dict:
        return self.run("pane", "get", pane_id)["result"]["pane"]

    def screen(self, pane_id: str) -> str:
        command = [str(self.binary), "--session", self.session or "", "agent", "read", AGENT,
                   "--source", "detection", "--format", "text"]
        return subprocess.run(command, capture_output=True, text=True, env=oasis_env(), timeout=30).stdout


@dataclass
class Report:
    version: str
    results: list[tuple[str, str, str]] = field(default_factory=list)  # (scenario, outcome, detail)

    def add(self, scenario: str, outcome: str, detail: str = "") -> None:
        self.results.append((scenario, outcome, detail))
        print(f"[{outcome}] {scenario}: {detail}", flush=True)

    @property
    def failed(self) -> bool:
        return any(outcome == "fail" for _, outcome, _ in self.results)

    def markdown(self) -> str:
        lines = [f"# Claude Code {self.version} against Oasis", ""]
        lines += [f"- **{outcome}** {scenario}: {detail}" for scenario, outcome, detail in self.results]
        return "\n".join(lines) + "\n"


def wait_for(what: str, probe, timeout: float, every: float = 2.0):
    deadline = time.monotonic() + timeout
    while True:
        value = probe()
        if value:
            return value
        if time.monotonic() > deadline:
            raise TimeoutError(what)
        time.sleep(every)


def transcript_entries(path: Path) -> list[dict]:
    entries = []
    try:
        text = path.read_text(errors="replace")
    except FileNotFoundError:
        return entries
    for line in text.splitlines():
        try:
            entries.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return entries


def goal_records(path: Path) -> list[dict]:
    return [
        entry["attachment"]
        for entry in transcript_entries(path)
        if entry.get("type") == "attachment"
        and isinstance(entry.get("attachment"), dict)
        and entry["attachment"].get("type") == "goal_status"
    ]


def tool_uses(path: Path, name: str) -> list[dict]:
    uses = []
    for entry in transcript_entries(path):
        content = (entry.get("message") or {}).get("content") if isinstance(entry.get("message"), dict) else None
        for block in content if isinstance(content, list) else []:
            if isinstance(block, dict) and block.get("type") == "tool_use" and block.get("name") == name:
                uses.append(block)
    return uses


class Check:
    def __init__(self, args: argparse.Namespace, report: Report, out: Path) -> None:
        self.args = args
        self.report = report
        self.out = out
        self.oasis = Oasis(args.oasis, SESSION)
        self.pane_id = ""
        self.transcript = Path()
        self.step = 0

    # --- plumbing -------------------------------------------------------

    def activity(self) -> dict:
        return self.oasis.pane(self.pane_id).get("background_activity") or {}

    def status(self) -> str:
        return self.oasis.pane(self.pane_id).get("agent_status", "unknown")

    def save_screen(self, label: str) -> str:
        self.step += 1
        text = self.oasis.screen(self.pane_id)
        (self.out / "screens" / f"{self.step:02d}-{label}.txt").write_text(text)
        return text

    def prompt(self, text: str, wait: bool = True, timeout: int = 300) -> None:
        args = ["agent", "prompt", AGENT, text]
        if wait:
            args += ["--wait", "--timeout", str(timeout * 1000)]
        self.oasis.run(*args, timeout=timeout + 30)

    def settle(self, timeout: float = 600) -> None:
        """Waits for the agent's turn, and any goal turns it chains, to end,
        answering any question it asks along the way."""
        def idle_for_a_while():
            status = self.status()
            if status == "blocked":
                self.answer_question()
                return False
            if status not in ("idle", "done"):
                return False
            time.sleep(6)
            return self.status() in ("idle", "done")
        wait_for("the agent to settle", idle_for_a_while, timeout, every=3)

    def answer_question(self) -> None:
        """Picks the option that keeps things in this session (Claude asks
        whether a /loop should become a cloud task), else the first."""
        screen = self.save_screen("question")
        options = []
        for line in screen.splitlines():
            text = line.strip().lstrip("❯").strip()
            number, _, label = text.partition(". ")
            if number.isdigit() and label:
                options.append((number, label.lower()))
        if not options:
            self.report.add("question", "inconclusive", "Claude is blocked on something that is not a question")
            raise TimeoutError("an unanswerable dialog")
        keep = next((n for n, label in options if "session" in label and "cloud" not in label), options[0][0])
        self.report.add("question", "note", f"answered option {keep} of {[l for _, l in options]}")
        self.oasis.run("agent", "send-keys", AGENT, keep)
        time.sleep(1)
        if self.status() == "blocked":
            self.oasis.run("agent", "send-keys", AGENT, "Enter")

    def shows(self, what: str, predicate, timeout: float = 20) -> dict | None:
        """What Oasis shows once `predicate` holds, or None after `timeout`.
        Showing nothing is `{}`, which still counts as holding."""
        try:
            return wait_for(what, lambda: (lambda a: [a] if predicate(a) else None)(self.activity()), timeout, 1)[0]
        except TimeoutError:
            return None

    # --- setup ------------------------------------------------------------

    def start(self) -> None:
        # A fresh folder per run, so Claude starts a new session rather than
        # resuming the last run's.
        work = CACHE / "work" / time.strftime("%Y%m%d-%H%M%S")
        work.mkdir(parents=True, exist_ok=True)
        self.forget_session()
        log = open(self.out / "server.log", "w")
        self.server = subprocess.Popen(
            [str(self.args.oasis), "--session", SESSION, "server"],
            stdout=log, stderr=subprocess.STDOUT, env=oasis_env(), start_new_session=True,
        )
        wait_for("the check server", lambda: self.oasis.run("workspace", "list", check=False).get("result"), 30, 0.5)
        claude_dir = Path(self.args.claude_dir)
        created = self.oasis.run(
            "workspace", "create", "--cwd", str(work), "--label", "claude-check", "--no-focus",
            "--env", f"PATH={claude_dir}:{os.environ.get('PATH', '/usr/bin:/bin')}",
        )
        self.pane_id = created["result"]["root_pane"]["pane_id"]
        time.sleep(2)
        try:
            self.oasis.run(
                "agent", "start", AGENT, "--kind", "claude", "--pane", self.pane_id, "--timeout", "90000",
                "--", "--model", MODEL, "--dangerously-skip-permissions", timeout=120,
            )
        except RuntimeError as error:
            # A first run in the work folder asks whether to trust it, with
            # "No, exit" selected.
            if "agent_not_ready" not in str(error):
                raise
            visible = self.oasis.run("pane", "read", self.pane_id, "--source", "visible",
                                     "--format", "text", check=False)
            if "trust this folder" in json.dumps(visible):
                self.oasis.run("pane", "send-keys", self.pane_id, "Down", "Enter")
            wait_for("Claude to start", lambda: self.status() == "idle", 60)
        session = wait_for(
            "Claude's session id",
            lambda: (self.oasis.pane(self.pane_id).get("agent_session") or {}).get("value"),
            60,
        )
        version = subprocess.run([str(claude_dir / "claude"), "--version"], capture_output=True, text=True).stdout
        if self.report.version not in version:
            raise RuntimeError(f"expected Claude {self.report.version}, the pane runs {version.strip()}")
        self.session_id = session

    def find_transcript(self) -> Path:
        projects = Path(os.environ.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude")) / "projects"
        return wait_for(
            "the session transcript",
            lambda: next(iter(projects.glob(f"*/{self.session_id}.jsonl")), None),
            60,
        )

    def forget_session(self) -> None:
        """Stops and deletes the check session, which would otherwise restore
        the last run's panes and agent."""
        host = Oasis(self.args.oasis, None)
        host.run("session", "stop", SESSION, check=False, timeout=30)
        host.run("session", "delete", SESSION, check=False, timeout=30)

    def stop(self) -> None:
        if not hasattr(self, "server"):
            return
        try:
            self.oasis.run("server", "stop", check=False, timeout=30)
        finally:
            try:
                self.server.wait(timeout=30)
            except Exception:
                self.server.kill()
            self.forget_session()
        shutil.rmtree(self.out / "transcript", ignore_errors=True)
        if self.transcript.is_file():
            shutil.copy(self.transcript, self.out / "transcript.jsonl")
            subagents = self.transcript.with_suffix("") / "subagents"
            if subagents.is_dir():
                shutil.copytree(subagents, self.out / "transcript" / "subagents", dirs_exist_ok=True)

    # --- scenarios --------------------------------------------------------

    def paused_notices(self) -> int:
        return sum(
            1 for entry in transcript_entries(self.transcript)
            if entry.get("subtype") == "informational"
            and str(entry.get("content", "")).startswith("Goal paused")
        )

    def goal_outcome(self, name: str, goal: str, outcome: str, flag: str, timeout: float) -> None:
        before = len(goal_records(self.transcript)) if self.transcript.is_file() else 0
        self.prompt(f"/goal {goal}", wait=False)
        if not self.transcript.is_file():
            self.transcript = self.find_transcript()
        # Claude pauses a goal its checks keep finding unmet and waits for a
        # message; it may give up on it once told to continue.
        for _ in range(3):
            paused = self.paused_notices()
            def ended_or_paused():
                record = next((r for r in goal_records(self.transcript)[before:]
                               if r.get("met") or r.get("failed")), None)
                return record or (self.paused_notices() > paused and {"paused": True})
            try:
                found = wait_for(f"the goal to end ({outcome})", ended_or_paused, timeout, 3)
            except TimeoutError:
                break
            if not found.get("paused"):
                break
            self.settle()
            shown = self.activity()
            screen = self.save_screen(f"{name}-paused")
            self.check_goal_screen(name, screen)
            notice = next((l.strip() for l in screen.splitlines() if "/goal" in l), "no goal notice")
            self.report.add(f"{name}: paused", "note", f"screen shows {notice!r}; Oasis shows {shown}")
            self.prompt("continue", wait=False)
        try:
            record = wait_for(
                f"the goal to end ({outcome})",
                # Setting a goal writes met:false marked sentinel; it ends met,
                # failed, or met:true marked sentinel when cleared.
                lambda: next(
                    (r for r in goal_records(self.transcript)[before:]
                     if r.get("met") or r.get("failed")),
                    None,
                ),
                timeout, 3,
            )
        except TimeoutError:
            self.report.add(name, "inconclusive", f"Claude did not end the goal within {timeout:.0f} s")
            self.prompt("/goal clear", wait=False)
            return
        ended = "met" if record.get("met") and not record.get("sentinel") else "failed" if record.get("failed") else "cleared"
        if ended != outcome:
            self.report.add(name, "inconclusive", f"Claude ended the goal as {ended}, not {outcome}")
        shown = self.shows(f"Oasis to show {flag}", lambda a: a.get(flag) and not a.get("goal"))
        screen = self.save_screen(name)
        if ended == outcome:
            if shown:
                self.report.add(name, "pass", f"Claude recorded {outcome}; Oasis shows {flag}")
            else:
                self.report.add(name, "fail", f"Claude recorded {outcome} ({record}); Oasis shows {self.activity()}")
        self.check_goal_screen(name, screen)
        self.settle()
        self.prompt("thanks, that's all")
        cleared = self.shows("the outcome to clear", lambda a: not a.get(flag) and not a.get("goal"))
        self.save_screen(f"{name}-after-prompt")
        if cleared is not None:
            self.report.add(f"{name}, then a typed prompt", "pass", "Oasis clears the outcome")
        else:
            self.report.add(f"{name}, then a typed prompt", "fail", f"Oasis still shows {self.activity()}")

    def check_goal_screen(self, name: str, screen: str) -> None:
        for line in screen.splitlines():
            text = line.strip()
            words = text.split(maxsplit=2)
            if (text.startswith(GOAL_GLYPHS) and len(words) > 1 and words[1] in ("Goal", "/goal")
                    and not text.startswith(KNOWN_GOAL_SCREEN)):
                self.report.add(f"{name} screen", "fail", f"a goal line Oasis does not read: {text!r}")

    def background_shell(self) -> None:
        name = "background shell"
        self.prompt(
            "Use the Bash tool with run_in_background set to true to run `sleep 240`. "
            "Do not wait for it. Then reply with the single word started."
        )
        self.settle(180)
        uses = [u for u in tool_uses(self.transcript, "Bash") if (u.get("input") or {}).get("run_in_background")]
        if not uses:
            self.report.add(name, "inconclusive", "Claude did not start a background shell")
            return
        shown = self.shows("Oasis to count the shell", lambda a: a.get("shells", 0) >= 1)
        self.save_screen("shell")
        self.report.add(name, "pass" if shown else "fail",
                        "Oasis counts the shell" if shown else f"Oasis shows {self.activity()}")

    def background_subagent(self) -> None:
        name = "background subagent"
        self.prompt(
            "Use the Agent tool with subagent_type general-purpose and run_in_background set to true. "
            "Tell the subagent to run `sleep 75` with the Bash tool and then reply done. "
            "Do not wait for it. Then reply with the single word started."
        )
        self.settle(180)
        metas = self.transcript.with_suffix("") / "subagents"
        running = lambda: [m for m in metas.glob("*.meta.json") if '"finished":true' not in m.read_text()]
        if not tool_uses(self.transcript, "Agent"):
            self.report.add(name, "inconclusive", "Claude did not start a subagent")
            return
        shown = self.shows("Oasis to count the subagent", lambda a: a.get("agents", 0) >= 1, 30)
        self.save_screen("subagent")
        try:
            records = wait_for("its record", running, 30, 1)
        except TimeoutError:
            records = []
        if shown and records:
            self.report.add(name, "pass", "Oasis counts the running subagent")
        else:
            self.report.add(name, "fail", f"records unfinished: {len(records)}; Oasis shows {self.activity()}")
        try:
            wait_for("the subagent to finish", lambda: not running(), 240, 5)
        except TimeoutError:
            self.report.add(f"{name} finishing", "inconclusive", "the subagent did not finish in time")
            return
        self.settle(240)
        gone = self.shows("Oasis to stop counting it", lambda a: not a.get("agents"), 60)
        self.save_screen("subagent-finished")
        self.report.add(f"{name} finishing", "pass" if gone is not None else "fail",
                        "Oasis stops counting it" if gone is not None else f"Oasis shows {self.activity()}")

    def repeating_loop(self) -> None:
        name = "repeating /loop"
        self.prompt("/loop 1h reply with the single word tick")
        self.settle(240)
        if not (tool_uses(self.transcript, "CronCreate") or tool_uses(self.transcript, "ScheduleWakeup")):
            self.report.add(name, "inconclusive", "Claude scheduled nothing")
            return
        shown = self.shows("Oasis to show the loop", lambda a: a.get("looping") or a.get("loop_next"), 30)
        self.save_screen("loop")
        self.report.add(name, "pass" if shown else "fail",
                        f"Oasis shows the loop ({shown.get('loop_next', '')})" if shown else f"Oasis shows {self.activity()}")
        self.prompt("Stop the loop: delete every scheduled job and arm no more wakeups. Reply stopped.")
        self.settle(240)
        stopped = self.shows("the loop to end", lambda a: not a.get("looping") and not a.get("loop_next"), 30)
        self.save_screen("loop-stopped")
        self.report.add(f"{name} stopped", "pass" if stopped is not None else "fail",
                        "Oasis shows no loop" if stopped is not None else f"Oasis shows {self.activity()}")

    def unknown_records(self) -> None:
        name = "records Oasis knows"
        problems = []
        for record in goal_records(self.transcript):
            new = set(record) - KNOWN_GOAL_KEYS
            if new:
                problems.append(f"goal record fields {sorted(new)}")
        for entry in transcript_entries(self.transcript):
            content = entry.get("content")
            if entry.get("subtype") == "informational" and isinstance(content, str) and content.startswith("Goal"):
                if not content.startswith(KNOWN_GOAL_NOTICES):
                    problems.append(f"goal notice {content[:60]!r}")
        if problems:
            self.report.add(name, "fail", "; ".join(sorted(set(problems))))
        else:
            self.report.add(name, "pass", "no goal record fields or notices Oasis does not know")

    def run(self) -> None:
        shutil.rmtree(self.out / "screens", ignore_errors=True)
        (self.out / "screens").mkdir(parents=True, exist_ok=True)
        try:
            self.start()
            self.goal_outcome("goal met", "Reply with exactly the word ready and nothing else.", "met",
                              "goal_achieved", 300)
            self.goal_outcome(
                "goal given up on",
                "The file proof.txt in this folder holds a valid proof that 1 equals 2 in ordinary "
                "arithmetic. You may not create, edit or delete any file.",
                "failed", "goal_failed", 300,
            )
            self.background_shell()
            self.background_subagent()
            self.repeating_loop()
            self.unknown_records()
        finally:
            self.stop()


def latest_installed_claude() -> str:
    output = subprocess.run(["mise", "where", "claude@latest"], capture_output=True, text=True).stdout.strip()
    return Path(output).name


def report_to_oasis(args: argparse.Namespace, report: Report, out: Path) -> None:
    """Opens a tab in the user's Oasis with Claude briefed to fix Oasis."""
    oasis = Oasis(args.oasis, None)
    workspaces = oasis.run("workspace", "list")["result"]["workspaces"]
    workspace = next((w["workspace_id"] for w in workspaces if w.get("label") == "oasis"), None)
    if workspace:
        pane = oasis.run("tab", "create", "--workspace", workspace, "--cwd", str(REPO),
                         "--label", f"claude {report.version}", "--no-focus")["result"]["root_pane"]["pane_id"]
    else:
        pane = oasis.run("workspace", "create", "--cwd", str(REPO), "--label", "oasis",
                         "--no-focus")["result"]["root_pane"]["pane_id"]
    time.sleep(2)
    name = f"fix-claude-{report.version.replace('.', '-')}"
    oasis.run("agent", "start", name, "--kind", "claude", "--pane", pane, "--timeout", "90000",
              "--", "--dangerously-skip-permissions", timeout=120)
    failures = "\n".join(f"- {s}: {d}" for s, o, d in report.results if o == "fail")
    oasis.run("agent", "prompt", name,
              f"Claude Code {report.version} was just installed, and the live check of Oasis against it "
              f"failed:\n{failures}\n\nEverything from the run is in {out}: report.md, the session's "
              f"transcript.jsonl and subagent records, the screens at each step in screens/, and "
              f"server.log. Find what this Claude version does that Oasis misreads. Add corpus cases "
              f"from that run with scripts/claude_corpus.py (see tests/fixtures/claude-corpus/README.md), "
              f"fix Oasis, prove the new tests red without the fix and green with it, then follow the "
              f"repo's commit, push and install routine. Rerun scripts/claude_live_check.py --claude "
              f"{report.version} at the end.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--claude", help="the Claude Code version to check (default: the latest installed)")
    parser.add_argument("--oasis", type=Path, default=Path.home() / ".local/bin/oasis")
    parser.add_argument("--report-to-oasis", action="store_true",
                        help="on failure, open a tab in the user's Oasis with Claude briefed to fix it")
    parser.add_argument("--force", action="store_true", help="check again a version already checked")
    args = parser.parse_args(argv)

    version = args.claude or latest_installed_claude()
    out = CACHE / version
    if (out / "report.md").exists() and not args.force:
        print(f"Claude {version} was already checked: {out / 'report.md'}")
        return 0
    install = subprocess.run(["mise", "where", f"claude@{version}"], capture_output=True, text=True)
    if install.returncode != 0:
        subprocess.run(["mise", "install", f"claude@{version}"], check=True)
        install = subprocess.run(["mise", "where", f"claude@{version}"], capture_output=True, text=True, check=True)
    args.claude_dir = install.stdout.strip()
    out.mkdir(parents=True, exist_ok=True)

    report = Report(version)
    try:
        Check(args, report, out).run()
    except Exception as error:  # the run itself broke; say so in the report
        report.add("check run", "fail", f"{type(error).__name__}: {error}")
    (out / "report.md").write_text(report.markdown())
    print(out / "report.md")
    if report.failed and args.report_to_oasis:
        report_to_oasis(args, report, out)
    return 1 if report.failed else 0


if __name__ == "__main__":
    sys.exit(main())

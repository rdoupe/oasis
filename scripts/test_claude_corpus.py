from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import claude_corpus
from scripts.claude_corpus import Scrambler


class ScramblerTest(unittest.TestCase):
    def test_words_keep_their_length_and_case(self) -> None:
        text = Scrambler().text("Alex deployed PROJ-1419 to buildbox.")
        self.assertEqual(len(text), len("Alex deployed PROJ-1419 to buildbox."))
        self.assertRegex(text, r"^[A-Z][a-z]{3} [a-z]{8} [A-Z]{4}-1419 to [a-z]{8}\.$")
        for word in ("Alex", "deployed", "PROJ", "buildbox"):
            self.assertNotIn(word, text)

    def test_claude_chrome_and_ids(self) -> None:
        scrambler = Scrambler()
        self.assertEqual(
            scrambler.text("✔ Goal achieved (14h · 62 turns · 439.3k tokens)"),
            "✔ Goal achieved (14h · 62 turns · 439.3k tokens)",
        )
        self.assertEqual(
            scrambler.text("✘ Goal could not be achieved (2h · 5 turns · 185.5k tokens)"),
            "✘ Goal could not be achieved (2h · 5 turns · 185.5k tokens)",
        )
        self.assertEqual(
            scrambler.text("Scheduled recurring job d7a8a2b4 (Every 30 minutes)"),
            "Scheduled recurring job d7a8a2b4 (Every 30 minutes)",
        )
        self.assertTrue(scrambler.text("mail someone@example.org").endswith(" <email>"))
        uuid = "6bf177c5-1233-4dae-be34-210672bd8748"
        self.assertIn(uuid, scrambler.text(f"session {uuid}"))

    def test_records_keep_what_oasis_reads(self) -> None:
        entry = {
            "type": "user",
            "timestamp": "2026-10-26T06:21:07.802Z",
            "cwd": "/work/alex/project",
            "origin": {"kind": "human"},
            "message": {
                "content": [
                    {"type": "tool_use", "name": "CronCreate", "input": {"cron": "*/5 * * * *"}},
                    {"type": "text", "name": "Sam", "text": "x" * 500},
                ]
            },
            "/work/alex/notes.md": {"agentType": "my-reviewer"},
            "agentType": "general-purpose",
        }
        out = Scrambler().value(entry)
        self.assertEqual(out["type"], "user")
        self.assertEqual(out["timestamp"], entry["timestamp"])
        self.assertEqual(out["origin"], {"kind": "human"})
        self.assertNotIn("alex", out["cwd"])
        tool, text = out["message"]["content"]
        self.assertEqual((tool["name"], tool["input"]["cron"]), ("CronCreate", "*/5 * * * *"))
        self.assertNotEqual(text["name"], "Sam")
        self.assertEqual(len(text["text"]), claude_corpus.LONG_TEXT + 1)
        self.assertNotIn("/work/alex/notes.md", out)
        self.assertNotIn("my-reviewer", json.dumps(out))
        self.assertEqual(out["agentType"], "general-purpose")


class TranscriptCommandTest(unittest.TestCase):
    def test_excerpt_keeps_line_numbers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            session = root / "session.jsonl"
            session.write_text(
                "\n".join(
                    [
                        json.dumps({"type": "user", "version": "2.1.310", "message": {"content": "one"}}),
                        json.dumps({"type": "api-request-blob", "body": "ScheduleWakeup docs"}),
                        '{"type":"assistant","message":',
                        json.dumps({"type": "system", "subtype": "turn_duration"}),
                    ]
                )
                + "\n"
            )
            with mock.patch.object(claude_corpus, "CORPUS", root / "corpus"):
                claude_corpus.main(["transcript", str(session), "--lines", "1:4", "--case", "sample"])
            case = root / "corpus" / "sample"
            lines = (case / "transcript.jsonl").read_text().splitlines()
            self.assertEqual(len(lines), 4)
            self.assertEqual(json.loads(lines[1]), {"type": "api-request-blob", "omitted": "request copy"})
            self.assertEqual(json.loads(lines[3])["subtype"], "turn_duration")
            self.assertIn('claude = "2.1.310"', (case / "case.toml").read_text())


if __name__ == "__main__":
    unittest.main()

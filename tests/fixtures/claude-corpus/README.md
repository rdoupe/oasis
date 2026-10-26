# Claude Code corpus

Anonymized excerpts of real Claude Code sessions, each with what Oasis should
read from it: goals and loops from the transcript, unfinished subagents from
their records beside it (`transcript/subagents/`), and background work from
the screen. `just claude-corpus` replays them all.

Each case's `case.toml` names the Claude Code version it came from, says what
happened, and lists the expected reads. Expectations come from the source
session (its records and what its screen showed), never from what Oasis
reported at the time.

## After a Claude Code upgrade

1. Run `just claude-corpus` to see whether the new version still reads the
   same way on the old cases.
2. Add cases from sessions on the new version, covering what changed or what
   the release notes touch:

   ```sh
   # A transcript excerpt; lines are 1-based and inclusive.
   scripts/claude_corpus.py transcript ~/.claude/projects/<project>/<session>.jsonl \
       --lines 120:180 --case goal-met-2-1-320
   # The session's subagent records, when the case counts subagents.
   scripts/claude_corpus.py subagents ~/.claude/projects/<project>/<session>.jsonl \
       --case goal-met-2-1-320
   # A screen; read it while the pane is idle so nothing scrolls.
   oasis agent read <pane> --source detection --format text > screen.txt
   scripts/claude_corpus.py screen screen.txt --case goal-met-2-1-320 --claude 2.1.320
   ```

3. Fill in `case.toml` from the source session, then rerun.

The script replaces every word of the session's own text with a random word
of the same length and case, keeps what Claude Code itself writes and the
fields Oasis reads, and cuts long text at 200 characters. Before committing,
search the new files for names, hosts and paths anyway.

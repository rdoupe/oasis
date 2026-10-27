#!/bin/sh
# Runs when mise installs a Claude Code version (the tool's `postinstall`) and
# starts scripts/claude_live_check.py against it in the background. A failing
# postinstall fails the install, so this always returns at once and succeeds.
#
# Install as ~/.local/bin/oasis-claude-upgrade-hook and name it in the mise
# config:
#
#   claude = { version = "latest", postinstall = "$HOME/.local/bin/oasis-claude-upgrade-hook" }
#
# On a machine without the Oasis source, set OASIS_CHECK_HOST to an SSH host
# that has it; the version is handed to the hook there. OASIS_REPO names the
# checkout (default ~/Work/oasis). OASIS_CHECK_DRY=1 only logs what would run.

version=${MISE_TOOL_VERSION:-${1:-}}
log_dir=${XDG_STATE_HOME:-$HOME/.local/state}/oasis-claude-check
mkdir -p "$log_dir" 2>/dev/null
log=$log_dir/hook.log
# What the hook says on stderr shows in the terminal running `mise upgrade`.
say() { printf 'Oasis: %s\n' "$1" >&2; }
error() { printf '\033[1;41;97m ERROR \033[0m \033[1;31mOasis: %s\033[0m\n' "$1" >&2; }
[ -n "$version" ] || { echo "$(date -Is) no version given" >>"$log"; error "no Claude version given; nothing checked"; exit 0; }
# The terminal running the upgrade, so a failed check can say so there.
tty=$(ps -o tty= -p $$ 2>/dev/null | tr -d ' ')
case $tty in pts/* | tty*) tty=/dev/$tty ;; *) tty= ;; esac

if [ -n "${OASIS_CHECK_HOST:-}" ]; then
    echo "$(date -Is) Claude $version installed; handing the check to $OASIS_CHECK_HOST" >>"$log"
    [ -n "${OASIS_CHECK_DRY:-}" ] && exit 0
    ssh -o BatchMode=yes -o ConnectTimeout=10 "$OASIS_CHECK_HOST" \
        "OASIS_CHECK_DRY=${OASIS_CHECK_DRY:-} \$HOME/.local/bin/oasis-claude-upgrade-hook $version" \
        >>"$log" 2>&1 </dev/null &
    exit 0
fi

repo=${OASIS_REPO:-$HOME/Work/oasis}
check="$repo/scripts/claude_live_check.py"
if [ ! -f "$check" ]; then
    echo "$(date -Is) Claude $version installed; no Oasis checkout at $repo" >>"$log"
    error "no Oasis checkout at $repo; Claude $version was not checked"
    exit 0
fi
unit="oasis-claude-check-$(printf '%s' "$version" | tr -c 'A-Za-z0-9' '-')"
echo "$(date -Is) Claude $version installed; starting $unit" >>"$log"
[ -n "${OASIS_CHECK_DRY:-}" ] && { say "would check Claude $version (dry run)"; exit 0; }
set -- python3 -I "$check" --claude "$version" --report-to-oasis
[ -n "$tty" ] && set -- "$@" --tell-tty "$tty"
if command -v systemd-run >/dev/null 2>&1 &&
    systemd-run --user --collect --quiet --unit "$unit" --working-directory "$repo" --setenv=PATH="$PATH" \
        "$@" >>"$log" 2>&1; then
    say "checking Claude $version in the background (about 10 minutes); a failure shows here and opens a tab to fix Oasis"
    exit 0
fi
# No user service manager: detach by hand.
if (cd "$repo" && setsid nohup "$@" >>"$log_dir/$unit.log" 2>&1 </dev/null &); then
    say "checking Claude $version in the background (about 10 minutes); a failure shows here and opens a tab to fix Oasis"
else
    echo "$(date -Is) could not start the check" >>"$log"
    error "could not start the check of Claude $version; see $log"
fi
exit 0

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
[ -n "$version" ] || { echo "$(date -Is) no version given" >>"$log"; exit 0; }

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
    exit 0
fi
unit="oasis-claude-check-$(printf '%s' "$version" | tr -c 'A-Za-z0-9' '-')"
echo "$(date -Is) Claude $version installed; starting $unit" >>"$log"
[ -n "${OASIS_CHECK_DRY:-}" ] && exit 0
if command -v systemd-run >/dev/null 2>&1 &&
    systemd-run --user --collect --quiet --unit "$unit" --working-directory "$repo" --setenv=PATH="$PATH" \
        python3 -I "$check" --claude "$version" --report-to-oasis >>"$log" 2>&1; then
    exit 0
fi
# No user service manager: detach by hand.
(cd "$repo" && setsid nohup python3 -I "$check" --claude "$version" --report-to-oasis \
    >>"$log_dir/$unit.log" 2>&1 </dev/null &)
exit 0

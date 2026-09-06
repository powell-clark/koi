#!/usr/bin/env bash
# Reap leaked duplicate evaluation-publish-cli.js processes (MSG-EGLPK026/027).
#
# WHY: a consciousness-plugin defect (CCC TASK-CCC31373, the Loop Stop hook
# re-fires while waiting on a Monitor task) leaks evaluation-publish-cli.js
# processes at ~1.1-1.8 GB each, several per session. They OOM-killed a CCC
# builder's push four times on an already-correct fix. The real fix is CCC's;
# this stops the bleeding until it lands.
#
# WHAT IS SAFE TO KILL — two rules, both from the cockpit's ruling:
#   1. A process for a session that ALSO has a newer process for the same
#      session (the older one is the leaked duplicate).
#   2. A process older than THRESHOLD_S that has been reparented away from its
#      spawning session — ppid 1 (systemd) or the user systemd (`systemd
#      --user`), whose session has therefore already exited. These are the ones
#      the plugin's own Stop-hook reap misses.
# A first, live instance whose session still owns it is NEVER killed. Killing a
# stale publish-cli is safe regardless: the plugin re-runs it on the next real
# publish.
#
# Dry-run by default; --apply actually sends the signal. SIGTERM first, since a
# publish that is genuinely mid-write should get the chance to finish.
#
# Usage: eval-cli-reap.sh [--apply] [threshold_seconds]

set -uo pipefail

APPLY=0
THRESHOLD_S="${EVAL_CLI_REAP_THRESHOLD_S:-120}"

for arg in "$@"; do
  case "$arg" in
    --apply) APPLY=1 ;;
    ''|*[!0-9]*) ;;
    *) THRESHOLD_S="$arg" ;;
  esac
done

MARKER="telemetry/evaluation-publish-cli.js"

# pid ppid rss_kb age_s session_id — session id is the second-to-last argv field.
snapshot() {
  ps -eo pid=,ppid=,rss=,etimes=,args= 2>/dev/null | awk -v marker="$MARKER" '
    index($0, marker) == 0 { next }
    {
      pid = $1; ppid = $2; rss = $3; age = $4;
      session = $(NF - 1);
      printf "%s %s %s %s %s\n", pid, ppid, rss, age, session
    }'
}

# The user systemd is where an orphaned user process reparents on this box, so
# "orphaned" means ppid 1 OR ppid of `systemd --user`, not ppid 1 alone.
user_systemd_pid() {
  pgrep -u "$(id -u)" -f 'systemd --user' 2>/dev/null | head -1
}

USER_SYSTEMD="$(user_systemd_pid)"
USER_SYSTEMD="${USER_SYSTEMD:-0}"

reaped=0
freed_kb=0

# Newest age per session wins: anything older for the same session is a duplicate.
newest_age_for_session() {
  local want="$1"
  snapshot | awk -v s="$want" '$5 == s { if (min == "" || $4 < min) min = $4 } END { print (min == "" ? -1 : min) }'
}

while read -r pid ppid rss age session; do
  [ -n "${pid:-}" ] || continue

  reason=""

  newest="$(newest_age_for_session "$session")"
  if [ "$newest" -ge 0 ] && [ "$age" -gt "$newest" ]; then
    reason="duplicate: a newer publish (age ${newest}s) exists for session ${session}"
  elif [ "$age" -gt "$THRESHOLD_S" ] && { [ "$ppid" = "1" ] || [ "$ppid" = "$USER_SYSTEMD" ]; }; then
    reason="orphaned (ppid ${ppid}) and older than ${THRESHOLD_S}s"
  fi

  [ -n "$reason" ] || continue

  rss_mb=$(( rss / 1024 ))
  if [ "$APPLY" -eq 1 ]; then
    if kill -TERM "$pid" 2>/dev/null; then
      echo "reaped pid ${pid} (${rss_mb} MB, age ${age}s, session ${session}) — ${reason}"
      reaped=$(( reaped + 1 ))
      freed_kb=$(( freed_kb + rss ))
    else
      echo "could not signal pid ${pid} (gone already, or not ours)" >&2
    fi
  else
    echo "WOULD reap pid ${pid} (${rss_mb} MB, age ${age}s, session ${session}) — ${reason}"
    reaped=$(( reaped + 1 ))
    freed_kb=$(( freed_kb + rss ))
  fi
done < <(snapshot)

freed_mb=$(( freed_kb / 1024 ))
if [ "$APPLY" -eq 1 ]; then
  echo "eval-cli-reap: reaped ${reaped} process(es), ~${freed_mb} MB resident released"
else
  echo "eval-cli-reap: ${reaped} candidate(s), ~${freed_mb} MB (dry run — pass --apply to act)"
fi

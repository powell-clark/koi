#!/usr/bin/env bash
# Sample every 10s for processes over an RSS threshold and record their full argv.
#
# WHY (INC-KOI031/032): earlyoom keeps killing "node" processes at 3-4.7 GB, but
# they are transient — a ps snapshot taken between kills shows nothing above
# 1.5 GB, so the class responsible has never been identified. A seat averages
# ~300 MB, so a 4 GB node process is something else: a subagent, a typecheck
# hook, esbuild, vitest, or a seat mid-spike. This records which.
#
# WHY A SERVICE, NOT A BACKGROUND TASK (WORK-KOI126): the first run of this
# script was terminated after 46 minutes by Claude Code's own background-task
# watchdog citing low memory, while earlyoom, the kernel OOM killer and
# systemd-oomd had all fired zero times. That watchdog keys off host-wide
# pressure, not the task's footprint, so any diagnostic launched from a session
# dies exactly when it is most needed. A systemd --user unit is outside it.
#
# Read-only: samples /proc via ps, writes one TSV line per offender per sample,
# and one journal line the FIRST time a given pid crosses the threshold.
# Changes nothing, kills nothing.
#
# Usage: big-proc-watch.sh [threshold_mb] [duration_s]
#   duration_s 0 (the default) means run until stopped.

set -uo pipefail

THRESHOLD_MB="${1:-2000}"
DURATION_S="${2:-0}"
OUT="${BIG_PROC_WATCH_OUT:-$HOME/.local/state/koi/big-proc-watch.tsv}"
INTERVAL_S="${BIG_PROC_WATCH_INTERVAL:-10}"

# Bound the TSV so a long-lived watcher cannot become its own disk-growth story.
MAX_LINES="${BIG_PROC_WATCH_MAX_LINES:-5000}"
KEEP_LINES=$(( MAX_LINES / 2 ))

mkdir -p "$(dirname "$OUT")" 2>/dev/null || true
[ -s "$OUT" ] || printf 'timestamp\trss_mb\tpid\tetime\tcommand\n' > "$OUT"

declare -A seen
threshold_kb=$(( THRESHOLD_MB * 1024 ))
deadline=0
[ "$DURATION_S" -gt 0 ] && deadline=$(( $(date +%s) + DURATION_S ))

echo "big-proc-watch: threshold ${THRESHOLD_MB} MB, interval ${INTERVAL_S}s, out ${OUT}"

while :; do
  if [ "$deadline" -gt 0 ] && [ "$(date +%s)" -ge "$deadline" ]; then
    break
  fi

  ts="$(date '+%Y-%m-%d %H:%M:%S')"

  # One ps call; awk emits "rss_kb pid etime argv" for every process over threshold.
  while IFS=$'\t' read -r rss_kb pid etime cmd; do
    [ -n "${pid:-}" ] || continue
    rss_mb=$(( rss_kb / 1024 ))
    printf '%s\t%s\t%s\t%s\t%.200s\n' "$ts" "$rss_mb" "$pid" "$etime" "$cmd" >> "$OUT"
    if [ -z "${seen[$pid]:-}" ]; then
      seen[$pid]=1
      printf 'OVER %s MB: pid %s rss %s MB etime %s :: %.160s\n' \
        "$THRESHOLD_MB" "$pid" "$rss_mb" "$etime" "$cmd"
    fi
  done < <(
    ps -eo rss=,pid=,etime=,args= --sort=-rss 2>/dev/null \
      | awk -v t="$threshold_kb" '
          $1 > t {
            rss=$1; pid=$2; et=$3;
            $1=""; $2=""; $3=""; sub(/^ +/, "");
            printf "%s\t%s\t%s\t%s\n", rss, pid, et, $0
          }'
  )

  # Rotate: keep the newest half once the cap is exceeded.
  if [ "$(wc -l < "$OUT")" -gt "$MAX_LINES" ]; then
    { head -n1 "$OUT"; tail -n "$KEEP_LINES" "$OUT" | grep -v '^timestamp'; } > "$OUT.tmp" \
      && mv "$OUT.tmp" "$OUT"
  fi

  sleep "$INTERVAL_S"
done

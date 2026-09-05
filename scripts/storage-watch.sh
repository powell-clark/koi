#!/usr/bin/env bash
# Watch personal Google storage before it fills again (TASK-KOI256).
#
# WHY: TASK-KOI255 freed 99.679 GiB from epowellclark@gmail.com by moving
# Photos ownership to the company account and repointing the Pixel 7a's
# backup target. Nothing enforced that repoint staying in place — a phone
# reset or a re-login to the Photos app can silently revert it, and Google's
# Photos Library API cannot split Gmail from Photos (both land in "Other"),
# so this watches the account total, not the product.
#
# `rclone about <remote>: --json` gives Total/Used/Trashed/Other/Free for an
# already-configured remote — no new credential, no OAuth flow. Thresholds
# are absolute GiB, not a fraction of quota, per the task's own pre-mortem:
# a percentage silently loosens if the account ever moves to a bigger plan.
#
# Read-only: one network call per configured account, its own state file,
# and — only above threshold — a journal line plus a desktop notification.
# A failed or rate-limited call keeps the previous good reading rather than
# overwriting it with a stale zero (same fix TASK-KOI192 needed for
# `rclone size`), and reports non-zero so the failure is visible in
# `systemctl --user status` rather than silently going quiet.
#
# Usage: storage-watch.sh [config_path]

set -uo pipefail

CONFIG="${1:-$HOME/.config/koi/storage.toml}"
STATE_DIR="${STORAGE_WATCH_STATE_DIR:-$HOME/.local/state/koi}"
STATE_FILE="$STATE_DIR/storage-watch.json"

mkdir -p "$STATE_DIR"
[ -s "$STATE_FILE" ] || echo '{}' > "$STATE_FILE"

if [ ! -f "$CONFIG" ]; then
  echo "storage-watch: no config at $CONFIG, nothing to watch" >&2
  exit 0
fi

config_json="$(python3 -c '
import sys, tomllib, json
with open(sys.argv[1], "rb") as f:
    print(json.dumps(tomllib.load(f)))
' "$CONFIG" 2>&1)" || { echo "storage-watch: failed to parse $CONFIG as TOML: $config_json" >&2; exit 1; }

warn_gib="$(echo "$config_json" | jq -r '.warn_threshold_gib // 5.0')"
critical_gib="$(echo "$config_json" | jq -r '.critical_threshold_gib // 10.0')"
accounts="$(echo "$config_json" | jq -c '.accounts // []')"
count=$(echo "$accounts" | jq 'length')

if [ "$count" -eq 0 ]; then
  echo "storage-watch: no [[accounts]] in $CONFIG" >&2
  exit 0
fi

status=0
for i in $(seq 0 $((count - 1))); do
  remote="$(echo "$accounts" | jq -r ".[$i].remote")"
  label="$(echo "$accounts" | jq -r ".[$i].label // .[$i].remote")"
  # Plain `// true` would also override an explicit `alert = false`, since
  # jq's alternative operator treats false the same as null/absent.
  alert="$(echo "$accounts" | jq -r "if .[$i].alert == null then true else .[$i].alert end")"
  ts="$(date '+%Y-%m-%d %H:%M:%S')"

  output="$(rclone about "${remote}:" --json 2>&1)"
  rc=$?
  if [ $rc -ne 0 ] || ! echo "$output" | jq -e . >/dev/null 2>&1; then
    echo "storage-watch: rclone about ${remote}: failed (rc=$rc), keeping last known reading for ${label} — ${output}" >&2
    status=1
    continue
  fi

  other_bytes=$(echo "$output" | jq -r '.other // 0')
  other_gib=$(awk -v b="$other_bytes" 'BEGIN { printf "%.3f", b / 1073741824 }')

  tmp="$(mktemp "$STATE_DIR/.storage-watch.XXXXXX")"
  jq --arg remote "$remote" --arg label "$label" --arg ts "$ts" \
     --argjson other_bytes "$other_bytes" --argjson other_gib "$other_gib" \
     '.[$remote] = {label: $label, timestamp: $ts, other_bytes: $other_bytes, other_gib: $other_gib}' \
     "$STATE_FILE" > "$tmp" && mv "$tmp" "$STATE_FILE"

  level="ok"
  if [ "$alert" = "true" ]; then
    awk -v g="$other_gib" -v c="$critical_gib" 'BEGIN { exit !(g >= c) }' && level="critical"
    [ "$level" = "ok" ] && awk -v g="$other_gib" -v w="$warn_gib" 'BEGIN { exit !(g >= w) }' && level="warn"
  fi

  if [ "$alert" != "true" ]; then
    echo "storage-watch: ${label} (${remote}) Other=${other_gib} GiB — tracked, no threshold (alert=false)"
  elif [ "$level" = "ok" ]; then
    echo "storage-watch: ${label} (${remote}) Other=${other_gib} GiB — ok (warn ${warn_gib} / critical ${critical_gib} GiB)"
  else
    msg="storage-watch: ${label} (${remote}) Other=${other_gib} GiB — ${level} (warn ${warn_gib} / critical ${critical_gib} GiB)"
    echo "$msg"
    command -v notify-send >/dev/null 2>&1 && notify-send -u normal "koi: Google storage ${level}" "$msg" || true
  fi
done

exit $status

#!/bin/bash
# koi-clamscan-full.sh — Monthly full ClamAV scan with exclusions
# Runs monthly as a defence-in-depth backstop after weekly delta scans
# Low priority (nice -n 19, ionice -c3) to avoid disrupting interactive work

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXCLUSIONS_FILE="${KOI_EXCLUSIONS_FILE:-${SCRIPT_DIR}/../config/clamscan-exclusions.yaml}"

# Logs go to koi's own data dir, not /var/log/koi. This runs as a --user
# timer, which cannot create or write a root-owned path: the previous
# /var/log/koi target made `mkdir -p` fail under `set -e`, so this script
# exited 1 before scanning anything and had never once completed a run
# (found 2026-09-06, TASK-KOI261). audits/ is where koi already keeps lynis
# output, and clamscan output is the same class of thing.
KOI_DATA_DIR="${KOI_DATA_DIR:-$HOME/.local/share/koi}"
CLAMSCAN_LOG="${KOI_CLAMSCAN_LOG:-${KOI_DATA_DIR}/audits/clamscan-full-$(date +%Y-%m-%d).log}"

# Overridable so the script can be exercised against a small tree; a real run
# scans $HOME and takes hours, which is untestable in practice.
SCAN_ROOT="${KOI_SCAN_ROOT:-${HOME}}"

# Ensure log directory exists
mkdir -p "$(dirname "$CLAMSCAN_LOG")"

# Helper: Log with timestamp
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "$CLAMSCAN_LOG"
}

log "Starting monthly full ClamAV scan (defence-in-depth)"
log "Exclusions: $EXCLUSIONS_FILE"
log "Log: $CLAMSCAN_LOG"

# Check if exclusions file exists
if [[ ! -f "$EXCLUSIONS_FILE" ]]; then
    log "WARNING: Exclusions file not found at $EXCLUSIONS_FILE"
    log "Running full scan without exclusions (will be slow)"
    EXCLUDE_ARGS=""
else
    # Build clamscan exclusion arguments from the exclusions file.
    #
    # clamscan takes POSIX REGEXES, not globs, and the exclusions file is
    # written in gitignore style, so passing those through verbatim is wrong
    # by clamscan's documented interface: `*.o` is not a valid anchored
    # extension match, and an unanchored `target` matches the word anywhere
    # in a path rather than a directory component.
    #
    # HONESTY NOTE (TASK-KOI261, 2026-09-06): this correction is by the
    # documentation, NOT by measurement. It cannot currently be verified on
    # this host, because clamscan run as this user returns "Can't open file
    # or directory" for every target tried — including a world-readable
    # /etc/hostname, from a plain shell AND from a transient systemd --user
    # unit. Cause not isolated. An earlier version of this comment claimed
    # the old patterns were measurably excluding everything; that claim was
    # disproved (scanning with zero exclusions also reports 0 files) and has
    # been removed rather than left to mislead.
    #
    # Translation: a `*.ext` glob becomes an anchored file-extension regex and
    # goes to --exclude; anything else is a directory name and goes to
    # --exclude-dir anchored on path separators, so "target" matches a
    # directory called target and not the word inside some other path.
    EXCLUDE_ARGS=()
    pattern_count=0
    while IFS= read -r line; do
        # Skip comments and empty lines
        [[ "$line" =~ ^# ]] && continue
        [[ -z "$line" ]] && continue
        # Reject patterns with metacharacters or leading dashes to prevent flag smuggling
        if [[ "$line" =~ [\$\`\;\|\&\<\>\(\)] ]] || [[ "$line" == -* ]]; then
            log "Skipping invalid exclusion pattern: $line"
            continue
        fi

        if [[ "$line" == \*.* ]]; then
            # *.log -> \.log$   (anchored extension match on the filename)
            ext="${line#\*}"
            EXCLUDE_ARGS+=("--exclude=${ext//./\\.}$")
        else
            # node_modules -> (^|/)node_modules(/|$)   (a real path component)
            escaped="${line//./\\.}"
            EXCLUDE_ARGS+=("--exclude-dir=(^|/)${escaped}(/|\$)")
        fi
        pattern_count=$((pattern_count + 1))
    done < "$EXCLUSIONS_FILE"
    log "Loaded ${pattern_count} exclusion patterns"
fi

# Run full scan
log "Scanning $SCAN_ROOT (this may take a while)..."
SCAN_START=$(date +%s)

if clamscan \
    --recursive \
    --log "$CLAMSCAN_LOG" \
    --verbose \
    "${EXCLUDE_ARGS[@]}" \
    "$SCAN_ROOT" 2>&1 | tee -a "$CLAMSCAN_LOG"; then

    SCAN_END=$(date +%s)
    SCAN_DURATION=$((SCAN_END - SCAN_START))
    log "Monthly full scan completed in ${SCAN_DURATION}s — no threats detected"
    exit 0
else
    SCAN_END=$(date +%s)
    SCAN_DURATION=$((SCAN_END - SCAN_START))
    CLAM_EXIT=$?
    log "Monthly full scan completed in ${SCAN_DURATION}s — threats or errors detected (exit code: $CLAM_EXIT)"
    exit $CLAM_EXIT
fi

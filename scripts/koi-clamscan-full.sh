#!/bin/bash
# koi-clamscan-full.sh — Monthly full ClamAV scan with exclusions
# Runs monthly as a defence-in-depth backstop after weekly delta scans
# Low priority (nice -n 19, ionice -c3) to avoid disrupting interactive work

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EXCLUSIONS_FILE="${KOI_EXCLUSIONS_FILE:-${SCRIPT_DIR}/../config/clamscan-exclusions.yaml}"
CLAMSCAN_LOG="/var/log/koi/clamscan-full-$(date +%Y-%m-%d).log"
SCAN_ROOT="${HOME}"

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
    # Build clamscan --exclude arguments from YAML file using array (safe from injection)
    EXCLUDE_ARGS=()
    while IFS= read -r line; do
        # Skip comments and empty lines
        [[ "$line" =~ ^# ]] && continue
        [[ -z "$line" ]] && continue
        # Reject patterns with metacharacters or leading dashes to prevent flag smuggling
        if [[ "$line" =~ [\$\`\;\|\&\<\>\(\)] ]] || [[ "$line" == -* ]]; then
            log "Skipping invalid exclusion pattern: $line"
            continue
        fi
        EXCLUDE_ARGS+=("--exclude=$line")
    done < "$EXCLUSIONS_FILE"
    log "Loaded ${#EXCLUDE_ARGS[@]} exclusion patterns"
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

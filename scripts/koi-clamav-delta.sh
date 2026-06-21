#!/bin/bash
# koi-clamav-delta.sh — Feed AIDE change list into clamscan for delta virus scanning
# Usage: koi-clamav-delta.sh [aide-report-path] [clamscan-log-path]
# Scans only files that changed since last AIDE database snapshot.

set -e

AIDE_REPORT="${1:-.}"
CLAMSCAN_LOG="${2:-/var/log/koi/clamscan-delta.log}"

# Ensure log directory exists
mkdir -p "$(dirname "$CLAMSCAN_LOG")"

# Helper: Log with timestamp
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "$CLAMSCAN_LOG"
}

log "Starting AIDE-driven delta ClamAV scan"

# Check if AIDE database exists
if [[ ! -f /var/lib/aide/aide.db.gz ]]; then
    log "AIDE database not initialized — falling back to monthly full scan path"
    log "Run 'sudo aideinit' to initialize AIDE database"
    exit 1
fi

# Generate AIDE changed files report
log "Running AIDE check to detect changes..."
AIDE_CHANGED_TMP=$(mktemp)
trap "rm -f $AIDE_CHANGED_TMP" EXIT

if ! sudo aide --check 2>&1 | grep -E "^/.*changed$" > "$AIDE_CHANGED_TMP"; then
    log "AIDE check failed or no changes detected"
    if [[ ! -s "$AIDE_CHANGED_TMP" ]]; then
        log "No file changes since last AIDE snapshot — skipping scan"
        exit 0
    fi
fi

# Count changed files
CHANGED_COUNT=$(wc -l < "$AIDE_CHANGED_TMP")
log "AIDE detected $CHANGED_COUNT changed files"

if [[ $CHANGED_COUNT -eq 0 ]]; then
    log "No changes detected — nothing to scan"
    exit 0
fi

# Run clamscan on changed files only
log "Starting clamscan on changed files (delta mode)..."
SCAN_START=$(date +%s)

if clamscan --file-list "$AIDE_CHANGED_TMP" \
    --recursive \
    --log "$CLAMSCAN_LOG" \
    --verbose 2>&1 | tee -a "$CLAMSCAN_LOG"; then

    SCAN_END=$(date +%s)
    SCAN_DURATION=$((SCAN_END - SCAN_START))
    log "Scan completed in ${SCAN_DURATION}s — no threats detected"
    exit 0
else
    SCAN_END=$(date +%s)
    SCAN_DURATION=$((SCAN_END - SCAN_START))
    CLAM_EXIT=$?
    log "Scan completed in ${SCAN_DURATION}s — threats or errors detected (exit code: $CLAM_EXIT)"
    exit $CLAM_EXIT
fi

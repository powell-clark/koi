# Google Cloud Storage Setup for Koi Metrics

## Overview

Koi automatically backs up metrics to Google Cloud Storage (GCS) for long-term retention.

**Local retention:** Max 1GB, 90 days
**GCS retention:** Unlimited, 1-2 years
**Backup frequency:** Daily at 2am

---

## Prerequisites

1. **Google Cloud account** with billing enabled
2. **gsutil** command-line tool installed
3. **GCS bucket** created for metrics

---

## Setup Steps

### 1. Install Google Cloud SDK

```bash
# Add Cloud SDK repo
echo "deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" | sudo tee -a /etc/apt/sources.list.d/google-cloud-sdk.list

# Import Google Cloud public key
curl https://packages.cloud.google.com/apt/doc/apt-key.gpg | sudo apt-key --keyring /usr/share/keyrings/cloud.google.gpg add -

# Install
sudo apt update && sudo apt install google-cloud-sdk
```

### 2. Authenticate with Google Cloud

```bash
gcloud auth login
gcloud config set project YOUR_PROJECT_ID
```

### 3. Create GCS Bucket

```bash
# Create bucket (replace with your preferred location)
gsutil mb -l europe-west2 gs://koi-metrics

# Set lifecycle policy (optional - auto-delete after 730 days / 2 years)
cat > lifecycle.json <<EOF
{
  "lifecycle": {
    "rule": [
      {
        "action": {"type": "Delete"},
        "condition": {"age": 730}
      }
    ]
  }
}
EOF

gsutil lifecycle set lifecycle.json gs://koi-metrics
```

### 4. Test Upload

```bash
# Test gsutil works
echo "test" > /tmp/test.txt
gsutil cp /tmp/test.txt gs://koi-metrics/test.txt
gsutil rm gs://koi-metrics/test.txt
```

### 5. Enable Backup Timer

```bash
# Copy systemd files
cp systemd/koi-metrics-backup.* ~/.config/systemd/user/

# Reload and enable
systemctl --user daemon-reload
systemctl --user enable koi-metrics-backup.timer
systemctl --user start koi-metrics-backup.timer

# Check status
systemctl --user status koi-metrics-backup.timer
```

---

## How It Works

**Daily at 2am:**

1. **Upload yesterday's metrics** to GCS
   - Compresses file with gzip (~80% smaller)
   - Uploads to `gs://koi-metrics/YYYY-MM/YYYY-MM-DD.jsonl.gz`
   - Deletes compressed copy after upload

2. **Compress old local files** (>7 days)
   - Reduces local storage by ~80%
   - Keeps recent data uncompressed for fast access

3. **Cleanup old local files**
   - Deletes files older than 90 days
   - Enforces 1GB maximum local storage
   - Oldest files deleted first if over limit

---

## Storage Estimates

**Local (max 1GB):**
- ~90 days of metrics
- Mix of compressed (>7 days) and uncompressed (<7 days)
- Compressed: ~10MB/month
- Uncompressed: ~50MB/month

**GCS (unlimited, 2 year auto-delete):**
- ~600MB per year compressed
- ~1.2GB for 2 years
- Cost: ~£0.02/month (Standard storage)

---

## Manual Operations

**Upload a specific date:**
```bash
koi backup upload --bucket koi-metrics --date 2025-11-20
```

**Force local cleanup:**
```bash
koi backup cleanup --keep-days 30 --max-size 0.5GB
```

**Compress old local files:**
```bash
koi backup compress --older-than 7d
```

---

## Bucket Structure

```
gs://koi-metrics/
├── 2025-11/
│   ├── 2025-11-01.jsonl.gz
│   ├── 2025-11-02.jsonl.gz
│   └── ...
├── 2025-12/
│   └── ...
└── lifecycle.json
```

---

## Cost Estimation

**GCS Standard Storage:**
- £0.023 per GB/month (europe-west2)
- 1.2GB for 2 years = ~£0.03/month
- Annual cost: ~£0.36

**Negligible for long-term metrics backup.**

---

## Troubleshooting

**Check timer status:**
```bash
systemctl --user list-timers | grep koi-metrics-backup
```

**View logs:**
```bash
journalctl --user -u koi-metrics-backup.service -f
```

**Test upload manually:**
```bash
koi backup upload --bucket koi-metrics
```

**Verify bucket contents:**
```bash
gsutil ls -lh gs://koi-metrics/
```

---

## Security

**Authentication:** Uses gcloud credentials (OAuth 2.0)
**Permissions:** Requires `storage.objects.create` on bucket
**Data:** Metrics contain system stats only (no secrets)

---

**Last Updated:** 2025-11-22

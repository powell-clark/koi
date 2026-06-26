# Koi Backup and Recovery

This document describes koi's backup infrastructure, configuration, and restore procedures.

## Overview

Koi implements a 3-tier data classification system with corresponding backup strategies:

| Tier | Classification | Backup Strategy | Location |
|------|---|---|---|
| Red | Secrets, credentials | Encrypted hardware key + backup (separate physical device) | Not covered here |
| Amber | Sensitive, non-secret | Encrypted off-site via rclone crypt | Google Drive (encrypted) |
| Green | Public, low-sensitivity | Standard backups acceptable | Local backups, public archives |

This document focuses on amber-tier encrypted backup using rclone crypt.

## Amber-Tier Encrypted Backup (rclone crypt)

### Architecture

The backup uses a two-layer rclone setup:

1. **Base Remote** (`gdrive`): Google Drive connection to your archive
2. **Crypt Remote** (`koi-crypt`): Encryption layer on top of the base remote

Files uploaded through `koi-crypt` are encrypted client-side before transmission to Google Drive. The Google Drive storage sees only opaque ciphertext and cannot decrypt files without the passphrase.

```
┌─────────────────────┐
│  Amber-tier Data    │
│  (plaintext)        │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│  koi-crypt Remote   │
│  (encryption layer) │
└──────────┬──────────┘
           │ (encrypted)
           ▼
┌─────────────────────┐
│ gdrive              │
│  (Google Drive)     │
└─────────────────────┘
```

### Configuration

#### Secrets Storage

Encryption keys and passphrases are stored outside the rclone configuration file in the koi secrets directory:

```
~/.config/koi/secrets/
├── backup-crypt-passphrase  (openssl rand -base64 32)
└── backup-crypt-salt        (openssl rand -base64 16)
```

These files have restrictive permissions (600) and are NOT checked into version control.

#### Remote Configuration

The rclone crypt remote is configured with:

- **Type**: crypt
- **Remote**: `gdrive:/Archive/encrypted`
- **Passphrase**: Read from `~/.config/koi/secrets/backup-crypt-passphrase`
- **Salt**: Read from `~/.config/koi/secrets/backup-crypt-salt`

View the current configuration:
```bash
rclone config show koi-crypt
rclone listremotes
```

### Backup Operations

#### Manual Backup

Upload amber-tier data to the encrypted remote:

```bash
rclone copy ~/amber-tier-data koi-crypt:/
```

Verify the backup completed:

```bash
# View plaintext filenames (through encryption layer)
rclone ls koi-crypt:/

# View ciphertext filenames (on actual Google Drive)
rclone ls gdrive:/Archive/encrypted
```

#### Scheduled Backup (systemd timer)

Backups can be scheduled via systemd user timers.

### Restore Procedures

#### Restore from Crypt Remote

To restore amber-tier data from the encrypted backup:

```bash
# List available backups
rclone ls koi-crypt:/

# Restore to a local directory
rclone copy koi-crypt:/path/to/data ~/restored-data
```

#### Verify Integrity

After restoring, verify data integrity using checksums:

```bash
# Generate hash of original data
md5sum ~/amber-tier-data/* > /tmp/original.md5

# Generate hash of restored data
md5sum ~/restored-data/* > /tmp/restored.md5

# Compare
diff /tmp/original.md5 /tmp/restored.md5
```

All hashes must match. If mismatches are found, investigate the restore process before trusting the backup.

## Disaster Recovery

In case of data loss:

1. **Verify Google Drive access**: Confirm credentials are valid and the encrypted backup is accessible
2. **Locate secrets**: Ensure `~/.config/koi/secrets/backup-crypt-passphrase` and salt are available
3. **Restore rclone config** (if necessary): Run `rclone config create koi-crypt crypt ...` with the stored secrets
4. **Restore data**: Use `rclone copy koi-crypt:/` to recover files
5. **Verify integrity**: Run checksums as described above

## Security Considerations

- **Passphrase storage**: The passphrase is stored locally in `~/.config/koi/secrets/`, not in the repo or rclone.conf
- **Key derivation**: rclone crypt uses PBKDF2 (password-based key derivation)
- **Encryption**: AES-256-GCM in Encrypt mode
- **Cloud exposure**: Google Drive sees only encrypted data and cannot decrypt without the local passphrase
- **Backup scope**: Currently covers amber-tier data only. Red-tier (secrets) and green-tier data use separate strategies

## References

- **ADR-0004**: Data sensitivity tiering (red/amber/green)
- **ADR-0005**: No persistent cloud mounts
- **rclone crypt docs**: https://rclone.org/crypt/

---

**Last updated**: 2026-05-30
**Tested on**: Ubuntu 24.04, rclone v1.x

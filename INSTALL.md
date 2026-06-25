# Koi Installation

The daemon ships as its own `koi-daemon` binary; the `koi` CLI is a separate
binary for inspecting and controlling it. Install both. See
`crates/koi-daemon/INSTALL.md` for the daemon-crate-local copy of these steps.

## Linux (systemd)

1. Build: `cargo build --release --bin koi-daemon --bin koi`
2. Stage binaries: `mkdir -p ~/.local/bin && cp target/release/koi-daemon target/release/koi ~/.local/bin/`
3. Install systemd user service: `mkdir -p ~/.config/systemd/user && cp crates/koi-daemon/koi-daemon.service ~/.config/systemd/user/`
4. Enable and start: `systemctl --user daemon-reload && systemctl --user enable --now koi-daemon.service`
5. Verify: `systemctl --user status koi-daemon.service`

## macOS (launchd)

1. Build (on macOS directly — no cross-compilation recipe yet): `cargo build --release --bin koi-daemon --bin koi`
2. Stage binaries: `mkdir -p ~/.local/bin && cp target/release/koi-daemon target/release/koi ~/.local/bin/`
3. Install LaunchAgent plist (substitutes your home path into the template):
   ```bash
   mkdir -p ~/Library/LaunchAgents
   sed "s|/Users/YOURNAME|$HOME|g" crates/koi-daemon/com.powellclark.koi.daemon.plist \
     > ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist
   ```
4. Load agent (also runs immediately — `RunAtLoad=true`): `launchctl load ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist`
5. Verify: `launchctl list | grep koi`

To unload: `launchctl unload ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist`

## Automated Backup (Linux)

After setting up the rclone crypt remote (see `docs/backup.md`), enable weekly encrypted backups:

1. Enable backup timer: `systemctl --user enable --now koi-backup.timer`
2. Verify schedule: `systemctl --user list-timers koi-backup.timer`
3. Check logs: `journalctl --user -u koi-backup.service`

The backup runs every Sunday at 03:00 local time with low CPU/IO priority (`nice -n 19 ionice -c3`). Failed runs trigger a desktop notification.

## Windows

Pending: Service wrapper or scheduled task equivalent (planned for a future release).

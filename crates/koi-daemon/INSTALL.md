# koi-daemon install

## Linux (systemd user service)

```bash
# 1. Build
cargo build --release --bin koi-daemon

# 2. Stage binary (~/.local/bin is on PATH by default on modern systems)
mkdir -p ~/.local/bin
cp target/release/koi-daemon ~/.local/bin/

# 3. Install user service unit
mkdir -p ~/.config/systemd/user
cp crates/koi-daemon/koi-daemon.service ~/.config/systemd/user/

# 4. Enable + start
systemctl --user daemon-reload
systemctl --user enable --now koi-daemon.service

# 5. Tail logs
journalctl --user -u koi-daemon.service -f
```

Stop/disable:

```bash
systemctl --user disable --now koi-daemon.service
```

## macOS (launchd LaunchAgent)

```bash
# 1. Build (build on macOS directly — no cross-compilation recipe yet)
cargo build --release --bin koi-daemon

# 2. Stage binary
mkdir -p ~/.local/bin
cp target/release/koi-daemon ~/.local/bin/

# 3. Install LaunchAgent plist (edit the ProgramArguments path to your user)
mkdir -p ~/Library/LaunchAgents
sed "s|/Users/YOURNAME|$HOME|g" crates/koi-daemon/com.powellclark.koi.daemon.plist \
  > ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist

# 4. Load (also runs immediately because RunAtLoad=true)
launchctl load ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist

# 5. Tail logs
tail -f ~/Library/Application\ Support/koi/logs/koi-daemon.log.*
# or stderr/stdout via launchd-captured files:
tail -f /tmp/koi-daemon.err /tmp/koi-daemon.out
```

Stop/unload:

```bash
launchctl unload ~/Library/LaunchAgents/com.powellclark.koi.daemon.plist
```

## Windows

Not yet supported. Tauri v2 tray + Task Scheduler integration is a future task.

## Paths (both platforms)

```bash
koi paths    # prints db, data_dir, cache_dir, logs_dir, etc.
```

On Linux: `~/.local/share/koi/` and `~/.cache/koi/`.
On macOS: `~/Library/Application Support/koi/` and `~/Library/Caches/com.powellclark.koi/`.

The daemon persists monitor reports to the SQLite DB reachable via
`koi paths`. Inspect the system state via `koi history`, `koi proposals`,
`koi approve`, `koi reject`, `koi report`, `koi stats`, `koi zones`.

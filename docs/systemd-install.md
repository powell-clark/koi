# Installing Koi Systemd Services

## Desktop Cleanup Timer (5 minutes)

The desktop cleanup timer runs every 5 minutes to move loose files from `~/Desktop` to `~/Documents/desktop-archive/{date}/`.

### Installation

```bash
# Copy service and timer files to user systemd directory
mkdir -p ~/.config/systemd/user/
cp deploy/systemd/koi-desktop-cleanup.service ~/.config/systemd/user/
cp deploy/systemd/koi-desktop-cleanup.timer ~/.config/systemd/user/

# Reload systemd daemon
systemctl --user daemon-reload

# Enable and start timer
systemctl --user enable koi-desktop-cleanup.timer
systemctl --user start koi-desktop-cleanup.timer
```

### Verify Installation

```bash
# Check timer status
systemctl --user status koi-desktop-cleanup.timer

# List all timers
systemctl --user list-timers

# View service logs
journalctl --user -u koi-desktop-cleanup.service -f
```

### Configuration

Edit `~/.config/koi/config.yaml` to configure desktop cleanup:

```yaml
desktop:
  dry_run: false  # Set to false to enable actual cleanup
  age_threshold: 300  # 5 minutes in seconds
```

### Uninstall

```bash
# Stop and disable timer
systemctl --user stop koi-desktop-cleanup.timer
systemctl --user disable koi-desktop-cleanup.timer

# Remove files
rm ~/.config/systemd/user/koi-desktop-cleanup.{service,timer}
systemctl --user daemon-reload
```

## Future Timers

Additional timers will be added for:
- 6-hour health reports
- Daily cache cleanup
- Weekly package updates check

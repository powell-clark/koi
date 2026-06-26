# Platform and Concurrency Notes

Koi is a Rust application that targets Linux, macOS, and Windows from a single
codebase. Most logic is platform-agnostic (`koi-core`); the notes below cover
where platform behaviour differs and how koi handles concurrent access to its
local database.

## Cross-Platform Behaviour

### What Works Everywhere

The `koi-core` crate is platform-agnostic:

- Metric collection via `sysinfo` (CPU, memory, swap, disk, network).
- The SQLite state database (same on-disk format on every platform).
- The disk, memory, cache, git, package, and filing monitors.

Metric collection and the SQLite schema are identical across platforms, so a
database written on one machine can be read on another.

### Platform-Specific Features

**Scheduling / automation:**

- Linux uses systemd user timers.
- macOS uses launchd agents.
- Windows uses Task Scheduler.

**Deep system metrics:**

- Linux PSI (pressure stall information) and systemd-oomd readout are Linux-only
  and are gated behind `cfg(target_os = "linux")`.
- Other platforms fall back to the cross-platform `sysinfo` metrics.

### Running on macOS

A one-off health check uses the same command as everywhere else:

```bash
koi check
```

To collect metrics on a schedule, install a launchd agent that invokes the koi
binary:

```xml
<!-- ~/Library/LaunchAgents/com.powellclark.koi.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.powellclark.koi</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/koi</string>
        <string>check</string>
    </array>
    <key>StartInterval</key>
    <integer>60</integer>
</dict>
</plist>
```

Load it with:

```bash
launchctl load ~/Library/LaunchAgents/com.powellclark.koi.plist
```

## Database Concurrency

Koi stores state in a single SQLite database in the platform data directory.

### WAL Mode

SQLite allows a single writer at a time. Koi opens its database in WAL
(Write-Ahead Logging) mode, which permits multiple concurrent readers alongside
a single writer and uses a busy-timeout so brief lock contention is handled
transparently. WAL is enabled by the `koi-core` state layer when the connection
is opened — callers do not configure it manually.

### Multiple koi Processes

The daemon and any CLI invocations may run concurrently:

- **Reads** (queries) are unlimited and never block.
- **Writes** serialise behind the busy-timeout; collisions are rare because the
  daemon writes on a low cadence.

### WAL Sidecar Files

WAL mode creates two sidecar files next to the database:

- `<db>-wal` — the write-ahead log.
- `<db>-shm` — shared-memory index.

Do not delete these while koi is running.

## Sensor Availability

Sensor coverage depends on the platform and, sometimes, on permissions.

- **Linux** — CPU, NVMe, and other temperatures are generally available through
  the kernel's hwmon interface; fan speeds depend on configuration.
- **macOS** — temperature and fan data require SMC access and may need elevated
  privileges or third-party tooling; readings can be unavailable by default.
- **Windows** — temperature exposure varies by hardware and driver support.

Sensor data is non-critical: koi's core monitoring (CPU, memory, disk, network)
works regardless of whether per-sensor temperature data is available.

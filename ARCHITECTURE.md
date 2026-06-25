# Koi Architecture

## System Overview

Koi is a persistent daemon that continuously observes the system through
event-driven monitors, maintains state in a local SQLite database, and surfaces
proposals through a native system-tray UI. It is a living system: always
present, stateful, self-improving, and consent-gated. The architecture is
designed around continuous observation and emergent value from ecosystem
health, not discrete CLI invocations.

Koi is built in Rust as a Cargo workspace and ships as a single native binary
per platform (Linux, macOS, Windows). The tray UI is built with Tauri v2. See
ADR-0013 for the architecture decision and rationale.

## Living System Properties

These are architectural constraints, not metaphors. Every component satisfies
them.

1. **Always present** — koi runs as a background daemon with a system-tray
   front end, not as a command the user invokes. Monitors observe continuously.
2. **Stateful** — koi keeps a local SQLite database of metric history, filing
   proposals, and the human decisions that train future proposals. State
   persists across restarts.
3. **Event-driven** — koi reacts to filesystem changes, resource pressure, and
   timer schedules rather than relying solely on periodic polling.
4. **Self-improving** — koi learns from approvals and rejections; filing and
   cleanup proposals become more accurate over time.
5. **Consent-gated** — actions on user files always require explicit approval;
   system cleanup follows tiered autonomy (see below).

## Workspace Layout

| Crate | Responsibility |
|-------|----------------|
| `koi-core` | Platform-agnostic domain logic: monitors, filing and consent model, cleaners, the classifier, and the SQLite state layer. No I/O orchestration — daemons and CLIs compose these primitives. |
| `koi-daemon` | The long-running process: scheduler, monitor cadence, filesystem-event wiring, and persistence to the local database. |
| `koi-cli` | Command-line front end for health checks, reports, and cleanup, sharing the same `koi-core` primitives as the daemon. |
| `koi-tray` | Tauri v2 system-tray application: status display and the consent UI where the user reviews and approves filing proposals. |
| `koi-plugins` | Plugin host exposing a stable API for user-defined rules and filters via WASM (heavy) and Rhai (lightweight scripting) extensions. |

## Core Components (`koi-core`)

### Monitors (`monitors/`)

Each monitor implements the `Monitor` trait and returns a structured report.
Health monitors are designed to complete quickly so they can run on a frequent
cadence; heavier scans (such as file classification) run on a slower schedule.

- **DiskMonitor** — directory growth tracking across accumulation directories,
  with a size cache and stale-cache detection by access time.
- **MemoryMonitor** — RAM and swap pressure via `sysinfo`, plus Linux PSI and
  systemd-oomd readout where available.
- **CacheMonitor** — known developer caches (npm, Playwright, pnpm, uv, and
  similar) with staleness detection and clean-command hints.
- **DockerMonitor** — container, image, and volume health by shelling to the
  docker CLI for portability across Docker Desktop, WSL, and colima.
- **GitMonitor** — uncommitted and unpushed state across local repositories via
  libgit2 (vendored, no system dependency).
- **PackageMonitor** — counts of outdated packages across the platform's package
  managers (apt on Linux, brew on macOS, npm and pip cross-platform).
- **NetworkMonitor** — interface counters via `sysinfo`; throughput deltas are
  derived downstream from persisted history.
- **LatencyMonitor** — TCP connect latency to configurable targets.

### Filing and Consent (`filing/`)

The file-lifecycle subsystem detects loose files in accumulation points and
proposes a home for each, never moving user files silently.

- **Scanners** — `downloads`, `documents`, and `inbox` monitors walk
  accumulation points and produce candidate items.
- **Classifier** — categorises each item and assigns a confidence score; lower
  confidence surfaces multiple candidate destinations for the user to choose.
- **Proposals** — every move is expressed as a reviewable proposal with a
  destination and rationale.
- **Managed zones** — directories marked with a `.koi-managed-by` file are
  treated as owned by another system (for example, a finance folder managed by
  an external finance system) and are skipped.
- **Executor** — applies only the proposals the user has approved and records
  an audit manifest.

### Cleaners (`cleaners/`)

Cleaners execute cleanup operations safely: dry-run by default, with size
estimation before action and confirmation for anything destructive. Reversible,
system-level cleanup (stale caches, reclaimable docker space) runs under higher
autonomy than anything touching user files.

### State (`state.rs`)

A local SQLite database (WAL mode) is the single source of truth for history and
learning. Three tables defined in ADR-0013 and ADR-0014:

- `monitor_reports` — time-series snapshots from the health monitors.
- `proposals` — pending, applied, and rejected file-lifecycle proposals.
- `decisions` — human approvals and rejections; the signal koi learns from.

Schema evolution uses SQLite's `user_version` pragma with typed migration
helpers.

## Data Flow

```
Health monitors → reports → analysis → suggestions → cleaners
      ↓             ↓          ↓            ↓            ↓
   Metrics      SQLite     Trends     Recommendations  Actions

File scanners → classifier → proposals → consent UI (tray) → executor
      ↓             ↓            ↓             ↓                ↓
  Scan results   Categories  Proposed moves  Approved set   Executed moves
```

## Autonomy Tiers

| Tier | Scope | Examples |
|------|-------|----------|
| Full autonomy | Safe, reversible, system-level | Cache cleanup, log analysis, metric collection, reports |
| Propose and approve | User files, configuration | Filing proposals, package upgrades, config changes |
| Human-initiated only | Destructive, irreversible | Deleting user files, major system changes |

## Configuration

Configuration is file-based and layered:

- **Thresholds** — warning and critical levels per monitor, staleness windows,
  and memory-pressure limits.
- **Exclusions** — directories to skip, safe-to-clean lists, and protected
  paths that koi must never touch.
- **Local overrides** — machine-specific adjustments and personal preferences,
  kept out of version control.

## Security Architecture

- **Consent gating** — any action on user files requires explicit approval;
  destructive operations are human-initiated only.
- **Service sandboxing** — the daemon runs with least privilege per platform
  (for example, systemd hardening on Linux: `NoNewPrivileges`, `PrivateTmp`,
  and read-only/inaccessible path lists scoped per service). See ADR-0003.
- **Data classification** — state and backups follow a tiered model that keeps
  sensitive material local and treats less-sensitive archives differently. See
  ADR-0004 and ADR-0005.
- **Local-first state** — koi's database and learning signal stay on the
  machine; nothing about the user's system is transmitted without consent.

## Design Principles

1. **Non-invasive** — monitoring must not measurably impact system performance.
2. **Safe by default** — dry-run mode, confirmations, and rollback where
   possible.
3. **Transparent** — show what will be cleaned or moved before acting.
4. **Configurable** — adapt to different workflows and machines.
5. **Informative** — explain why each recommendation is made.
6. **Understand before acting** — never reorganise files without understanding
   the existing structure.

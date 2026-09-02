# Koi

**A calm, honest housekeeper for your computer.** Koi is a small daemon-in-progress
that aims to keep your machine clean, organised, and healthy — continuously, and only
ever with your consent. It's early, and worth being precise about what that means: the
CLI and core monitors run today, the daemon and tray app are mid-build, and the whole
workspace builds and passes its tests on Linux, macOS and Windows in CI. What is not
yet shipped is *tested desktop support* — the tray has been exercised on Linux, the
Windows installer builds but is unsigned, and nothing has been run on real Mac
hardware.

[![CI (Linux, macOS, Windows)](https://github.com/powell-clark/koi/actions/workflows/ci.yml/badge.svg)](https://github.com/powell-clark/koi/actions/workflows/ci.yml)
[![secret-scan](https://github.com/powell-clark/koi/actions/workflows/secret-scan.yml/badge.svg)](https://github.com/powell-clark/koi/actions/workflows/secret-scan.yml)
[![License: FSL-1.1-Apache-2.0](https://img.shields.io/badge/license-FSL--1.1--Apache--2.0-blue)](LICENSE)
[![platforms: linux | macos | windows](https://img.shields.io/badge/platforms-linux%20%7C%20macos%20%7C%20windows-informational)](https://github.com/powell-clark/koi/actions/workflows/ci.yml)

The CI badge covers all three platforms: `fmt`, `clippy -D warnings` and the test
suite run on `ubuntu-latest`, `macos-latest` and `windows-latest` with
`fail-fast: false`, so the badge is green only when every platform is.

Named after koi fish: they live in the pond and keep the ecosystem clear as part of
their nature, while the pond's keeper sets the boundaries. You work, create files, run
projects. Koi tends the floor of the system so it stays a good place to work.

## Why Koi is different

Most "cleaner" apps make money by nagging you, scaring you, or bundling junk. Koi is
the opposite by design:

- **No API key. No AI tax. No cloud required.** The local client is fully functional and
  completely free, forever. Its intelligence is a local, on-device learner — not a
  metered call to someone's server.
- **Consent-gated.** Koi never moves or deletes your files without explicit approval. It
  proposes; you decide. Safe, reversible cache cleanup is the only thing it does on its
  own, and only from a conservative allow-list.
- **No nagware.** There is no upsell popup, no "your PC is at risk", no dark patterns.
  The business model (an optional paid cloud, below) structurally removes any incentive
  to nag.
- **Transparent.** Everything it observes and proposes is inspectable. State lives in a
  local SQLite database under your control.

## What it does

- **System health** — disk growth, memory pressure, stale caches (npm, Playwright,
  Docker, etc.), Docker sprawl, uncommitted/unpushed git across your projects, outdated
  packages.
- **File lifecycle** — scans your accumulation points (Downloads, Documents, Desktop),
  detects loose files, and proposes a home for each. It learns from what you approve and
  reject, so its suggestions get better over time. It respects "managed zones" owned by
  other tools and never touches them.
- **Safe cleanup** — reclaims space from caches and build sprawl, with a preview first.
- **One glance** — a tray icon (green/amber/red) and a popover summary, plus a fast CLI.

## Install

Koi is a single native binary per platform. Until packaged installers ship, build from
source (stable Rust toolchain required):

```bash
git clone https://github.com/powell-clark/koi
cd koi
cargo build --release
# the CLI is target/release/koi
```

Packaged installers for the tray app are built on demand by the
[`koi-tray` workflow](../../actions/workflows/koi-tray.yml) — a Debian `.deb`
on Linux and an NSIS `-setup.exe` on Windows, downloadable as run artefacts.
They are unsigned. macOS is not yet shipped: a `.dmg` builds in the same
workflow, but nothing on macOS has been installed or tested on real hardware.

## Usage

```bash
koi check        # run health diagnostics
koi report       # render a markdown health + proposals report
koi scan         # scan accumulation points and produce filing proposals
koi approve      # review and apply pending proposals (supports --dry-run, --limit)
koi clean        # reclaim safe caches (with a size preview)
koi stats        # proposal counts, decisions, learned destinations
koi history <monitor>   # recent readings for a monitor
```

Koi stores its state under your platform's standard data directory
(`~/.local/share/koi`, `~/Library/Application Support/koi`, or `%APPDATA%\koi`) and reads
configuration from `~/.config/koi`. It never writes into its own source tree.

## How it works

Koi is a Rust workspace: a core library, a CLI (working today), and an in-progress
daemon, Tauri v2 tray app, and plugin host for user-defined rules. Metrics and filing
decisions persist to local SQLite. The filing "learner" is a simple, inspectable
classifier that counts your approvals and rejections — no external inference in the
hot path.

## Open core — the optional paid cloud

The local client is free and keyless, always. A separate, **opt-in** hosted service (in
development) will offer cross-machine sync, a web dashboard, and Google Drive
organisation for a flat subscription. Any LLM-powered conveniences there are
**bring-your-own-key, pass-through** — your key, your provider bill, never resold. You
never need it to use Koi.

## Disclaimer

Koi is built to be safe by design — consent-gating, dry-run previews, and a
conservative allow-list for anything it does on its own, all covered by tests in CI.
That said, it touches your files and system configuration, and it's early software, so
back up anything irreplaceable as you normally would. Provided "as is", without
warranty of any kind; see [LICENSE](LICENSE) for the full terms.

## License

[Functional Source License v1.1, Apache 2.0 future license](LICENSE) (FSL-1.1-Apache-2.0).
Source-available and free for any non-competing use — individuals, internal company use,
self-hosting. Each release automatically becomes Apache-2.0 two years after it ships.

## Contributing

Issues and pull requests are welcome. CI runs format, lint (clippy, warnings denied),
tests, and a secret scan on every change across all three platforms.

---

© 2025-2026 Powell-Clark Limited.

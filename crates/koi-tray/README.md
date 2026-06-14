# koi-tray

Tauri v2 system-tray application for koi — surfaces system health at a glance and
the file-lifecycle consent UI (approve/reject proposals) without opening a terminal.

This crate is the desktop face of koi. The daemon (`koi-daemon`) does the work and
persists state to SQLite; `koi-tray` reads that state and presents it in the tray.

## Status

Functional. The tray icon colour reflects overall system health —
green/amber/red. Clicking the tray opens a popover showing a per-monitor health
summary and pending proposals, read live from koi-core's SQLite store, with
per-proposal approve/reject backed by the existing executor. Cross-platform CI
bundling is in place; installer packaging and autostart-on-login are planned.

## Prerequisites (Linux)

The webkit/soup system libraries are required to build:

```bash
sudo apt install libsoup-3.0-dev libwebkit2gtk-4.1-dev
```

macOS and Windows need no extra system packages (WebKit / WebView2 ship with the OS;
WebView2 is bundled by the installer on Windows).

## Build and run

Plain cargo (no extra tooling — produces the binary):

```bash
cargo run -p koi-tray            # launch the tray app
cargo build --release -p koi-tray # release binary at target/release/koi-tray
```

Via the Tauri CLI (adds hot-reload in dev and OS installers in build):

```bash
cargo install tauri-cli --version '^2'   # one-time
cargo tauri dev                          # run from crates/koi-tray/
cargo tauri build                        # bundle .deb/.AppImage (Linux), .dmg (macOS), nsis setup (Windows)
```

`cargo tauri` reads `tauri.conf.json` in this directory. The bundle icon set lives
in `icons/` (RGBA PNGs, required by Tauri's build script); the status-colour tray
icons are in `icons/status/`.

## Cross-platform CI builds

Every push to `main` that touches the tray, koi-core, or the workspace manifest
runs `.github/workflows/koi-tray.yml`, which builds the bundle on
`ubuntu-latest`, `macos-latest`, and `windows-latest` in parallel and uploads the
installers as workflow artifacts (retained 14 days). You can also trigger it
manually from the Actions tab ("Run workflow").

To grab a build to test on a real Mac or Windows box without cutting a release:

1. Open the repo on GitHub → **Actions** → **koi-tray** → the latest run.
2. Scroll to **Artifacts** and download the one for your OS:
   - `koi-tray-macos-latest` → `.dmg` (open it, drag to Applications)
   - `koi-tray-windows-latest` → `*-setup.exe` (run the NSIS installer)
   - `koi-tray-ubuntu-latest` → `.deb` / `.AppImage`
3. The artifacts are **unsigned** (code signing is planned). On macOS, right-click →
   Open to bypass Gatekeeper the first time; on Windows, accept the SmartScreen
   prompt.

The fast `ci.yml` workflow separately builds and tests the logic crates
(`koi-core`/`koi-cli`/`koi-daemon`/`koi-plugins`) on all three OSes on every push —
that is the quick cross-platform compile/test signal, independent of the GUI bundle.

## Layout

```
crates/koi-tray/
├── Cargo.toml          # tauri v2 deps (tray-icon, image-png), koi-core, rusqlite
├── build.rs            # tauri_build::build()
├── tauri.conf.json     # app config — popover window, frontendDist=ui, bundle targets
├── capabilities/       # Tauri v2 ACL — core IPC capability for the popover window
├── icons/              # RGBA PNG icon set (+ status/ green-amber-red tray colours)
├── ui/index.html       # the popover frontend (self-contained HTML/CSS/JS)
└── src/main.rs         # tray icon colour, popover toggle, IPC commands → koi-core
```

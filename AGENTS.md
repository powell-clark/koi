# Koi — Project Instructions

Rules only. State lives in the files each section references — never restate it here.

<living-system-properties>
Architectural constraints, not metaphors — every component must satisfy all five:
1. Always present — a daemon/tray app, not an invoked CLI; observes via filesystem events (inotify/FSEvents), systemd journal, and periodic collection.
2. Stateful — local SQLite holds state history, learned preferences, filing rules, and approval/rejection patterns; persists across restarts.
3. Event-driven — reacts to filesystem changes, resource pressure, session events, and timers; never polling-only.
4. Self-improving — learns from approvals and rejections; proposals become more accurate; thresholds adapt to observed usage.
5. Consent-gated — actions on user files always require explicit approval.
Koi learns: what the operator considers bloat versus necessary, thresholds that trigger concern, cleanup patterns that work, where file types belong, and which directories other systems manage.
</living-system-properties>

<autonomy-tiers>
Do without asking (safe, reversible, system-level): read logs and metrics, analyse trends, generate reports, run read-only diagnostics, clean items listed in config/exclusions.yaml safe_to_clean.
Propose and wait for approval (user files, configuration): filing proposals; package installs, removals, and upgrades; any system configuration change including sysctl and systemd units.
Human-initiated only (destructive or irreversible): deleting user files, major system changes, hardware changes.
Never block waiting on the operator — record what is needed and continue with other work.
</autonomy-tiers>

<state-sources>
- Machine profile, swap config, freeze runbook, maintenance cadence: data/host.yaml (gitignored) — read it before any system-level work
- Monitor thresholds: config/thresholds.yaml. Safe-to-clean and protected paths: config/exclusions.yaml
- Purpose, vision, mission: CONSCIOUSNESS/identity-vision-mission.md. Live roadmap: /consciousness:pgps
- Machine history: data/incidents.jsonl and data/worklog.jsonl (`koi incidents`, `koi worklog`)
</state-sources>

<incident-and-work-logs>
Append-only JSONL in data/ (gitignored). incidents.jsonl: INC-KOI### entries for crashes, freezes, OOM, hardware faults — evidence, cause analysis, preventive actions. worklog.jsonl: WORK-KOI### entries for tuning, cleanup, config changes, installs — with incident_ref when incident-triggered.
- Log an incident after any crash, freeze, or unexpected failure; on session start, check whether the previous boot ended uncleanly.
- Log one work entry per batch of related system changes.
- Never edit an existing entry — append a new one to update status.
</incident-and-work-logs>

<git-workflow-and-public-repo-safety>
The repo is public. main is protected (required PR, required gitleaks check, no force-push, no admin bypass). dev is unprotected day-to-day WIP — pull before starting, commit and push freely. PR dev→main only when work is verified against its task's acceptance criteria; systemd unit changes always need the operator's sign-off first. main receives only reviewed, working, complete increments.
- dev is exactly as public as main.
- The private layer (CONSCIOUSNESS/, data/, src/, operational docs) is gitignored deliberately. Never weaken those exclusions — a command that assumes gitignored content gets committed is stale, not the gitignore.
- Before staging anything outside the excluded paths, check whether it names a real vulnerability, misconfiguration, or exploit detail about this machine. Gitleaks catches credential-shaped secrets, not descriptive narrative.
Commit prefixes: feat, fix, chore, refactor, docs, test — wip:/attempt: until verified working. Authorship and attribution rules are global (AGENTS.md at ~/.llm-global/).
</git-workflow-and-public-repo-safety>

<monitors>
Seven monitors: DiskMonitor (directory growth), MemoryMonitor (oomd events, pressure), CacheMonitor (stale caches), DockerMonitor (unused images, volumes, containers), GitMonitor (uncommitted and unpushed work across projects), PackageMonitor (apt, npm, pip; brew on macOS), FileMonitor (accumulation points, filing proposals).
- Monitors 1–6 complete within 200ms combined; FileMonitor runs on its own slower schedule.
- Non-invasive; dry-run by default; always show what will be cleaned or moved before acting.
- FileMonitor never moves user files silently and respects managed zones (`.koi-managed-by` markers; `koi zones` lists them): desktop-archive automation, the Book's finance domain via ~/inbox/, git-managed ~/projects/.
</monitors>

<project-structure>
crates/ is the product — koi-core, koi-daemon, koi-cli, koi-tray, koi-plugins; `koi --help` is the authoritative command surface. src/koi/ is the Python prototype — reference only, read it, never extend it. Repo-root hooks/ and skills/ are pre-plugin legacy, wired to nothing.
</project-structure>

<style>
British English. Precise component names — DiskMonitor, not "disk thing".
</style>

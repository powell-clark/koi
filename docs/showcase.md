# Koi, in one cycle

This page follows a single file from the moment it lands in `Downloads` to the
moment it is filed, using the real CLI. Every command and every line of output
below was run against a scratch directory — no personal files, no real
statements, nothing from anyone's actual disk.

If you have not read the code, this is the shortest honest description of what
koi does: **it proposes, you decide, and only then does anything move.**

## What is shipped, and what is not

- **Linux** — the CLI and the monitors run today. This is where koi is used daily.
- **Windows** — builds and passes the full test suite in CI; the tray installer
  is produced unsigned by the packaging workflow.
- **macOS** — builds and passes tests in CI, and a `.dmg` is produced, but
  **nothing has been installed or exercised on real Mac hardware**. Treat it as
  unshipped.
- **The local client is free and needs no API key.** Its intelligence is a local
  classifier that counts what you approve and reject. There is no metered call
  to anyone's server, and no account is required to use it.

## The cycle

### 1. Four files land in Downloads

```
$ ls ~/Downloads
holiday-photo.jpg
Inter-Regular.ttf
Invoice ACME-0042.pdf
Monzo_bank_statement_2026-08.pdf
```

A bank statement, an invoice, a font, a photo. To an extension-based filer these
are two PDFs, a TTF and a JPG — which is how a statement and a takeaway receipt
end up in the same folder.

### 2. koi scans and proposes

```
$ koi scan
4 proposal(s) — stored; review with `koi proposals`, apply with `koi approve --all`:
  [6a6a8d08 90%] filename rule → ~/Documents/Finance/Statements/Monzo_bank_statement_2026-08.pdf
         from ~/Downloads/Monzo_bank_statement_2026-08.pdf
  [788a8fc3 90%] filename rule → ~/Documents/Finance/Invoices/Invoice ACME-0042.pdf
         from ~/Downloads/Invoice ACME-0042.pdf
  [a23ad5fc 90%] filename rule → ~/Documents/Fonts/Inter-Regular.ttf
         from ~/Downloads/Inter-Regular.ttf
  [8350ccc4 80%] Image file → ~/Documents/Media/Photos/holiday-photo.jpg
         from ~/Downloads/holiday-photo.jpg
```

Three of the four matched on **what the file is**, not its extension: the
statement went to `Finance/Statements` because the name carries an issuer, and
the invoice to `Finance/Invoices`. The photo matched no content rule and fell
through to the extension bucket, which is the designed fallback rather than a
failure.

Nothing has moved. These are proposals sitting in a local database.

### 3. You decide

```
$ koi approve 6a6a8d08
  ✓ 6a6a8d08 ~/Downloads/Monzo_bank_statement_2026-08.pdf

Applied: 1, Skipped: 0, Failed: 0
```

```
$ ls ~/Documents/Finance/Statements
Monzo_bank_statement_2026-08.pdf
```

One file moved, because one file was approved. The other three are still sitting
in `Downloads`, still proposed, waiting.

You can approve one at a time, review a batch with `--dry-run`, throttle with
`--limit`, or reject outright. Content-bearing documents are deliberately held
back from bulk approval, so a single command cannot sweep your financial and
medical records into a folder you have not looked at.

### 4. The decision is recorded

```
$ koi stats

| Monitor          | State   | Count |
|------------------|---------|-------|
| DownloadsMonitor | applied |     1 |
| DownloadsMonitor | pending |     3 |
```

Every approval and rejection is a row in a local SQLite database. That record is
what the classifier learns from: file enough statements to `Finance/Statements`
and koi grows more confident proposing it. The learning is yours, it stays on
your machine, and you can read it with `koi stats`.

## What else it watches

Filing is one half. The other is the machine itself — `koi check` reports disk
growth, memory pressure, stale caches, Docker sprawl, uncommitted and unpushed
work across your projects, outdated packages, whether koi's own scheduled units
are alive, and how old the pile in your inbox is getting.

`koi check` prints one line per monitor with a status and how long it took,
and a bullet under any that has something to say. Its output is shaped like the
transcripts above; it is not reproduced here because a real run reports this
machine's disks, memory and project list, and that is not something to publish.
Run it yourself to see yours.

Cleanup previews before it acts, and only ever touches a conservative allow-list
of caches that re-populate on next use. Your files are never in that list.

## The rule underneath all of it

Koi is consent-gated by design: **it never moves or deletes a file you have not
approved.** Safe, reversible cache cleanup is the single exception, and even
that shows you the size first. Anything koi does to a user file leaves a
decision row behind, and anything it puts in the trash comes back with
`koi trash restore`.

That constraint is not a setting. It is the thing the architecture is built
around, and it is why the interesting part of this project is the loop above
rather than the cleaning.

---

*Every command on this page was run against a scratch directory while writing
it. Reproduce it yourself: point `~/.config/koi/filing.toml` at a test folder,
drop some files in, and run `koi scan`.*

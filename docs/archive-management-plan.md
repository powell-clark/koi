# Documents/desktop-archive Management Plan

Intelligent Documents/desktop-archive management with categorisation, pruning, and space reclamation.

**Problem:** Files archived from Desktop currently sit in date-based folders (`~/Documents/desktop-archive/YYYY-MM-DD/`) indefinitely with no organisation, making them hard to find and wasting disk space.

**Solution:** Implement intelligent categorisation, automatic pruning, and space reclamation to make the archive useful and space-efficient.

---

## Three-Component Design

### 1. ArchiveCategoriser

**Purpose:** Transform date-based chaos into organised categories

**Input:** `~/Documents/desktop-archive/YYYY-MM-DD/` (flat date folders)

**Output:** Organised structure:
```
~/Documents/desktop-archive/
├── screenshots/
│   ├── 2025-11/
│   │   ├── 2025-11-28_00-11-36.png
│   │   └── 2025-11-28_01-16-32.png
│   └── 2025-10/
├── documents/
│   ├── pdfs/
│   ├── office/
│   └── text/
├── downloads/
│   ├── code/
│   ├── images/
│   └── archives/
├── projects/
│   ├── koi/
│   ├── webapp/
│   └── other/
└── uncategorised/
```

**Categorisation Logic:**

1. **By file type:**
   - Screenshots: `Screenshot from*.png`, `*.png` with "screenshot" metadata
   - Documents: `.pdf`, `.docx`, `.xlsx`, `.txt`, `.md`
   - Code: `.py`, `.js`, `.ts`, `.go`, `.rs`, etc.
   - Archives: `.zip`, `.tar.gz`, `.7z`
   - Images: `.jpg`, `.png`, `.gif` (non-screenshots)

2. **By project context (automatic detection):**
   - Scan filename for project names (`koi`, `webapp`, `notes`, etc.)
   - Check file path in clipboard history (if screenshot was taken while working on project)
   - Detect project-specific patterns (e.g., a file containing a project's issue-tracker tag)
   - Files with project context auto-moved to `projects/{name}/`

   **User control via filename:**
   - Add `#koi` to filename → auto-categorised to koi project
   - Add `#webapp` → webapp project
   - Add `#doc` → documents folder
   - Example: `Screenshot from 2025-11-28.png` → rename to `Screenshot from 2025-11-28 #koi.png`
   - Categoriser strips the tag and files correctly

3. **By date (within categories):**
   - Screenshots organised by month (`YYYY-MM/`)
   - Documents by year (`YYYY/`)
   - Keeps chronological context without chaos

**Execution:**
```bash
koi archive --categorise          # Dry-run (show what would change)
koi archive --categorise --apply  # Actually reorganise files
```

**Performance:** Complete in <500ms for typical archive (100-500 files)

---

### 2. ArchivePruner

**Purpose:** Reclaim disk space by removing/compressing old files

**Pruning Rules:**

1. **Screenshots (aggressive with protection):**
   - Delete screenshots older than 30 days
   - Rationale: Screenshots are usually temporary references
   - **Exceptions:**
     - Screenshots in `projects/` folders (assumed important)
     - Screenshots with `.keep` or `.star` suffix (user-flagged)
     - Screenshots renamed from default pattern (shows intent)

2. **Documents (conservative):**
   - Compress documents older than 90 days (gzip)
   - Delete obvious temp files (`Untitled*.txt`, `New Document*.docx`)
   - Keep all PDFs and office documents indefinitely

3. **Duplicates (smart detection):**
   - Hash-based duplicate detection
   - Keep newest, delete duplicates
   - Report savings

4. **Downloads (cautious):**
   - Flag downloads older than 60 days for manual review
   - Don't auto-delete (user might need them)

**Safety Features:**
- Dry-run by default
- Generate deletion manifest before acting
- Move to `.Trash/` instead of permanent deletion
- Undo capability (restore from trash within 30 days)

**Protection Mechanisms:**

Three ways to protect important screenshots/files from auto-deletion:

1. **Automatic project detection** - Categoriser detects project context and moves files automatically
   - Scans filename for project names (`koi`, `webapp`, etc.)
   - Detects project patterns (e.g., a project's issue-tracker tag)
   - Screenshots in `~/Documents/desktop-archive/projects/koi/` are never auto-deleted
   - **No manual action required** - happens during categorisation

2. **Rename the file** - Any screenshot renamed from default pattern is kept indefinitely
   - Example: `Screenshot from 2025-11-28 01-16-32.png` → `koi-architecture-diagram.png`
   - Shows intent to keep it

3. **Add .keep suffix** - Append `.keep` before extension to mark as important
   - Example: `mv screenshot.png screenshot.keep.png`
   - Quick command: `koi archive --keep <file>` (auto-renames)

**Interactive Review Mode:**

For users who want approval before deletion:

```bash
koi archive --review              # Interactive review of old files
```

Shows each file with:
- Thumbnail preview (for images)
- Age and size
- Detected project context (if any)
- Options: [k]eep [d]elete [p]roject [r]ename [s]kip [D]elete-all-similar [q]uit

Example workflow:
```
Reviewing 156 screenshots older than 30 days (1.8GB)

[1/156] Screenshot from 2025-10-15 14-23-45.png
        Age: 44 days | Size: 2.3MB
        Detected: might be related to 'koi' project
        [k]eep [d]elete [p]roject [r]ename [s]kip [D]elete-all-similar [q]uit? p

        → Moved to ~/Documents/desktop-archive/projects/koi/
        → Protected from auto-deletion ✓

[2/156] Screenshot from 2025-10-20 09-15-22.png
        Age: 39 days | Size: 1.1MB
        [k]eep [d]elete [p]roject [r]ename [s]kip [D]elete-all-similar [q]uit? D

        Delete all screenshots from 2025-10-20? (15 files, 18.3MB) [y/n]: y
        → Deleted 15 similar files ✓
        → Skipping to next date...

[17/156] Screenshot from 2025-10-18 14-15-33.png
        ...
```

**Batch operations:**
- `D` = Delete all screenshots from same day/hour
- `K` = Keep all screenshots from same day/hour
- `P` = Move all screenshots from same day to project

Speeds up review significantly when you have many screenshots from same session.

**Execution:**
```bash
koi archive --prune               # Show what would be deleted/compressed
koi archive --prune --apply       # Actually prune files (auto mode)
koi archive --review              # Interactive review before deletion
koi archive --prune --stats       # Show space savings
koi archive --prune --undo        # Restore last pruning operation
```

**Expected savings:** 50-70% space reduction for typical archive

---

### 3. Archive CLI Integration

**Purpose:** User-friendly interface for archive management

**New command:** `koi archive`

**Subcommands:**

```bash
# Categorisation
koi archive --categorise          # Preview organisation changes
koi archive --categorise --apply  # Apply changes

# Protection
koi archive --keep <file>         # Mark file to keep (adds .keep suffix)
koi archive --keep <pattern>      # Keep all matching files
koi archive --protect <dir>       # Protect entire directory

# Pruning
koi archive --prune               # Preview deletions/compressions
koi archive --prune --apply       # Apply pruning (automatic)
koi archive --review              # Interactive review mode
koi archive --prune --undo        # Restore last pruning

# Statistics
koi archive --stats               # Show archive breakdown
koi archive --stats --detailed    # Per-category analysis

# Combined
koi archive --clean               # Categorise + prune (dry-run)
koi archive --clean --apply       # Full cleanup (automatic)
koi archive --clean --review      # Full cleanup (interactive)
```

**Output Format (rich terminal):**

```
Archive Statistics
──────────────────
Total size: 3.5GB (1,234 files)

By Category:
  Screenshots     2.1GB (856 files)  [60%] ████████████
  Documents       0.8GB (234 files)  [23%] ████
  Downloads       0.4GB (89 files)   [11%] ██
  Projects        0.2GB (55 files)   [6%]  █

Pruning Potential:
  Screenshots >30d   1.8GB (723 files)  [DELETE]
  Duplicates         0.3GB (45 files)   [DELETE]
  Compressible docs  0.5GB (180 files)  [COMPRESS 80%]

Total reclaimable: 2.1GB (60% of archive)

Run: koi archive --clean --apply
```

---

## Quick Reference: Filename Tags

**Control categorisation by adding tags to filenames:**

```bash
# Project tags
#koi              → projects/koi/
#webapp    → projects/webapp/
#notes          → projects/notes/

# Category tags
#doc             → documents/
#code            → downloads/code/
#keep            → Protected from deletion (adds .keep suffix)

# Examples
Screenshot from 2025-11-28 14-32-15.png           # Normal: screenshots/2025-11/
Screenshot from 2025-11-28 14-32-15 #koi.png      # Tagged: projects/koi/
database-diagram #koi #keep.png                   # Tagged: projects/koi/ + protected
architecture-notes #doc.png                       # Tagged: documents/pdfs/
```

**How it works:**
1. You rename file to add tag: `Screenshot... #koi.png`
2. Categoriser detects tag during next run
3. Moves file to correct folder
4. Strips tag from final filename
5. Result: `Screenshot from 2025-11-28 14-32-15.png` in `projects/koi/`

**No need to remember exact folder paths** - just add `#projectname` and categoriser handles it.

**Dynamic folder creation:**

If you use a tag that doesn't exist in config (like `#pineapples`), the categoriser can:

1. **Auto-create mode** (default): Creates `~/Documents/desktop-archive/custom/pineapples/` automatically
2. **Ask mode**: Prompts "Create folder for #pineapples? [y/n/remember]"
3. **Ignore mode**: Leaves file in current location, logs unknown tag

You can also add tags to config manually:
```yaml
# Edit config/thresholds.yaml
archive:
  categorisation:
    tags:
      projects:
        pineapples: ~/Documents/desktop-archive/projects/pineapples/
```

Next categorisation run, `#pineapples` tag works automatically!

---

## Implementation Priority

**Phase 1 (MVP):**
1. Basic categorisation (Screenshots, Documents, Other)
2. Simple pruning (screenshots >30d)
3. CLI integration with dry-run

**Phase 2 (Enhanced):**
1. Project-based categorisation
2. Duplicate detection
3. Compression support
4. Undo capability

**Phase 3 (Advanced):**
1. ML-based categorisation
2. Smart retention policies
3. Archive search/indexing
4. Cloud backup integration

---

## Configuration

**Add to `config/thresholds.yaml`:**

```yaml
archive:
  categorisation:
    enabled: true
    project_detection: true

    # Filename tag support
    tags:
      enabled: true
      strip_after_categorise: true  # Remove #tag from filename after moving
      auto_create_folders: true     # Create new folders for unknown tags

      # Project mappings (add your own!)
      projects:
        koi: ~/Documents/desktop-archive/projects/koi/
        webapp: ~/Documents/desktop-archive/projects/webapp/
        notes: ~/Documents/desktop-archive/projects/notes/

      # Category mappings
      categories:
        doc: ~/Documents/desktop-archive/documents/
        code: ~/Documents/desktop-archive/downloads/code/
        keep: .keep  # Special: adds .keep suffix instead of moving

      # Unknown tag behaviour
      unknown_tags:
        action: create_folder  # 'create_folder', 'ignore', or 'ask'
        location: ~/Documents/desktop-archive/custom/  # Where to create new folders

  pruning:
    mode: review  # 'auto' or 'review' - default interactive review

    screenshots:
      max_age_days: 30
      keep_in_projects: true
      keep_renamed: true       # Keep screenshots renamed from default pattern
      keep_suffixes: ['.keep', '.star']  # Protected suffixes

    documents:
      compress_after_days: 90
      compression_level: 6  # gzip default

    duplicates:
      enabled: true
      hash_algorithm: sha256

    downloads:
      review_after_days: 60
      auto_delete: false

  safety:
    dry_run_default: true
    use_trash: true
    trash_retention_days: 30

  review:
    show_thumbnails: true    # Show image previews in terminal
    batch_size: 50           # Number of files per review session
    auto_skip_kept: true     # Skip files with .keep suffix

    # Batch operation grouping
    batch_similar_by: date   # 'date', 'hour', 'size', or 'none'
    batch_confirmation: true # Confirm before batch delete/keep
```

---

## Testing Strategy

**Unit tests:**
- File type detection accuracy
- Category assignment logic
- Pruning rules correctness
- Duplicate detection precision

**Integration tests:**
- Full archive categorisation
- Pruning with undo
- Performance benchmarks (<500ms)

**Manual testing:**
- Real archive (current ~/Documents/desktop-archive/)
- Verify no data loss
- Confirm space savings
- Check user experience

---

## Success Criteria

1. **Categorisation:**
   - 95% of files correctly categorised
   - <500ms execution time
   - Zero data loss

2. **Pruning:**
   - 50-70% space reclamation
   - 100% safe (trash + undo)
   - Clear user reporting

3. **User experience:**
   - Single command for full cleanup
   - Clear dry-run previews
   - Helpful statistics

---

## Timeline

**Week 1:** Implement ArchiveCategoriser
**Week 2:** Implement ArchivePruner
**Week 3:** CLI integration and testing

**Total:** 3 weeks to production-ready

---

**Created:** 2025-11-28
**Author:** Powell-Clark Limited
**Status:** Planning (waiting for approval)

#!/usr/bin/env bash
# Install koi git hooks into the active hooks directory WITHOUT disturbing existing
# hooks (notably the consciousness pre-commit validator). Idempotent: re-running is
# a no-op beyond refreshing the symlink. Refuses to clobber a foreign pre-push.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$repo_root"

# Resolve the hooks directory git actually consults (core.hooksPath if set, else
# the default .git/hooks). This preserves whatever else lives there.
hooks_dir="$(git config --get core.hooksPath || true)"
[ -z "$hooks_dir" ] && hooks_dir="$repo_root/.git/hooks"
mkdir -p "$hooks_dir"

src="$repo_root/githooks/pre-push"
dst="$hooks_dir/pre-push"

chmod +x "$repo_root/scripts/verify.sh" "$src"

# Only manage our own hook: install if absent, our symlink, or a koi-authored hook
# (identified by its reference to scripts/verify.sh). Never overwrite a foreign file.
if [ ! -e "$dst" ] || [ -L "$dst" ] || grep -q 'scripts/verify.sh' "$dst" 2>/dev/null; then
  ln -sfn "$src" "$dst"
  echo "installed pre-push hook: $dst -> $src"
else
  echo "WARNING: $dst exists and is not koi-managed; left untouched." >&2
  exit 1
fi

echo "koi git hooks installed (existing hooks preserved)."

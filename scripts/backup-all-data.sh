#!/usr/bin/env bash
# Consistent SQLite backups. Retains every backup; never restores or deletes data.
set -euo pipefail
STATE_DIR="${XLH_STATE_DIR:?Set XLH_STATE_DIR to the existing absolute state directory}"
[[ "$STATE_DIR" = /* ]] || { echo 'XLH_STATE_DIR must be absolute' >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo 'Install sqlite3 before backing up' >&2; exit 1; }
STAMP="$(date -u +%Y%m%dT%H%M%SZ)-$$"
DEST="$STATE_DIR/backups/all-$STAMP"
mkdir -p "$DEST"
count=0
for name in xlh realtime forecast; do
  db="$STATE_DIR/data/$name.db"
  [[ -f "$db" ]] || continue
  # Double single quotes for SQLite string literal paths.
  escaped="${DEST//\'/\'\'}"
  sqlite3 "$db" ".timeout 30000" ".backup '$escaped/$name.db'"
  [[ "$(sqlite3 "$DEST/$name.db" 'PRAGMA integrity_check;')" == ok ]] || { echo "Invalid backup: $name" >&2; exit 1; }
  count=$((count+1))
done
[[ $count -gt 0 ]] || { echo 'No databases found; check the existing state directory' >&2; exit 1; }
(cd "$DEST" && sha256sum ./*.db > SHA256SUMS)
echo "Verified $count databases in $DEST"

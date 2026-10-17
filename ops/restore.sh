#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
archive="${1:-}"
if [[ -z "$archive" || ! -f "$archive" || ! -f "$archive.sha256" ]]; then
  echo "Usage: bash ops/restore.sh ./backups/drawer-TIMESTAMP.tar.gz (and .sha256)" >&2
  exit 2
fi
archive="$(realpath "$archive")"
backupdir="$(realpath backups)"
if [[ "$(dirname "$archive")" != "$backupdir" ]]; then
  echo "Copy the backup and checksum into ./backups before restoring." >&2
  exit 2
fi
exec 9>backups/.maintenance.lock
flock -n 9 || { echo 'Another backup or restore is running.' >&2; exit 1; }
# Verify this exact archive, regardless of which filename its sidecar lists.
expected="$(awk 'NR == 1 {print $1}' "$archive.sha256")"
[[ "$expected" =~ ^[[:xdigit:]]{64}$ ]] || { echo 'Invalid checksum.' >&2; exit 1; }
actual="$(sha256sum "$archive")"
[[ "${actual%% *}" == "${expected,,}" ]] || { echo 'Checksum mismatch.' >&2; exit 1; }
echo "WARNING: this replaces the current Drawer store. Type RESTORE to proceed:"
read -r answer
[[ "$answer" == "RESTORE" ]] || exit 1

# Keep the writer stopped from the safety backup through the entire restore.
docker compose stop drawer
stamp="$(date -u +%Y%m%dT%H%M%S%N)"
safety="drawer-before-restore-$stamp.tar.gz"
# On failure leave the service stopped for inspection. Never start a partially moved store.
trap 'echo "Restore failed; Drawer remains stopped. Inspect the safety backup and pre-restore directory before starting it." >&2' EXIT
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c \
  'tar --exclude="./.maintenance.lock" --exclude="./users/*/.maintenance.lock" --exclude="./pre-restore-*" --exclude="./.restore-stage-*" -czf "/backups/$1" -C /data .' sh "$safety"
( cd backups && sha256sum "$safety" > "$safety.sha256" )
snapshot="pre-restore-$stamp"
stage=".restore-stage-$stamp"
# Pass filenames as arguments; never interpolate an archive name into shell code.
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c '
  set -eu
  archive=$1; snapshot=$2; stage=$3
  mkdir "/data/$stage"
  tar -xzf "/backups/$archive" -C "/data/$stage"
  # Old snapshots may contain a process lease; keep the live lock inode.
  if [ -f "/data/$stage/.maintenance.lock" ]; then unlink "/data/$stage/.maintenance.lock"; fi
  test -f "/data/$stage/drawer.db"
  test -d "/data/$stage/blobs"
  chown -R drawer:drawer "/data/$stage"
  mkdir "/data/$snapshot"
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers; do
    if [ -e "/data/$entry" ]; then mv "/data/$entry" "/data/$snapshot/"; fi
  done
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers; do
    if [ -e "/data/$stage/$entry" ]; then mv "/data/$stage/$entry" /data/; fi
  done
  rmdir "/data/$stage"
' sh "$(basename "$archive")" "$snapshot" "$stage"
docker compose up -d drawer >/dev/null
trap - EXIT
echo "Restore extracted. Previous store retained as /data/$snapshot."
echo "Safety backup: backups/$safety"
echo "Verify with: docker compose exec drawer drawer doctor"

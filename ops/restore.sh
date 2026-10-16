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
( cd backups && sha256sum -c "$(basename "$archive").sha256" )
echo "WARNING: this replaces the current Drawer store. Type RESTORE to proceed:"
read -r answer
[[ "$answer" == "RESTORE" ]] || exit 1

# Make a separate safety backup before modifying current data.
bash ops/backup.sh
docker compose stop drawer
trap 'docker compose up -d drawer >/dev/null' EXIT
snapshot="pre-restore-$(date -u +%Y%m%dT%H%M%SZ)"
stage=".restore-stage-$(date -u +%Y%m%dT%H%M%SZ)"
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c "
  set -eu
  mkdir -p '/data/$stage'
  tar -xzf '/backups/$(basename "$archive")' -C '/data/$stage'
  test -f '/data/$stage/drawer.db'
  test -d '/data/$stage/blobs'
  mkdir -p '/data/$snapshot'
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers; do
    if [ -e \"/data/\$entry\" ]; then mv \"/data/\$entry\" '/data/$snapshot/'; fi
  done
  for entry in drawer.db drawer.db-wal drawer.db-shm blobs transfers; do
    if [ -e \"/data/$stage/\$entry\" ]; then mv \"/data/$stage/\$entry\" /data/; fi
  done
  chown -R drawer:drawer '/data/drawer.db' '/data/blobs'
  if [ -d /data/transfers ]; then chown -R drawer:drawer /data/transfers; fi
"
echo "Restore extracted. Previous store retained as /data/$snapshot."
echo "Verify with: docker compose exec drawer drawer doctor"

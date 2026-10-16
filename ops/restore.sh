#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
archive="${1:-}"
if [[ -z "$archive" || ! -f "$archive" || ! -f "$archive.sha256" ]]; then
  echo "Usage: ops/restore.sh /absolute/path/to/drawer-backup.tar.gz (and .sha256)" >&2
  exit 2
fi
archive="$(realpath "$archive")"
backupdir="$(realpath backups)"
# We only allow restoral from the mounted backup directory.
if [[ "$(dirname "$archive")" != "$backupdir" ]]; then
  echo "Copy the backup and checksum into ./backups before restoring." >&2
  exit 2
fi
( cd backups && sha256sum -c "$(basename "$archive").sha256" )
echo "WARNING: this replaces the current Drawer store. Type RESTORE to proceed:"
read -r answer
[[ "$answer" == "RESTORE" ]] || exit 1
# Retain a full snapshot so a failed extraction is recoverable.
ops/backup.sh
docker compose stop drawer
trap 'docker compose up -d drawer >/dev/null' EXIT
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c \
  "mkdir -p /data/pre-restore && mv /data/drawer.db* /data/pre-restore/ 2>/dev/null || true; \
   if [ -d /data/blobs ]; then mv /data/blobs /data/pre-restore/blobs; fi; \
   if [ -d /data/transfers ]; then mv /data/transfers /data/pre-restore/transfers; fi; \
   tar -xzf '/backups/$(basename "$archive")' -C /data; \
   chown -R drawer:drawer /data"
echo "Restore extracted. Check with: docker compose exec drawer drawer doctor"

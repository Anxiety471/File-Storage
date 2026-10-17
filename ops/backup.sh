#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p backups
# Serialize maintenance across shell invocations, including restore.
exec 9>backups/.maintenance.lock
flock -n 9 || { echo 'Another backup or restore is running.' >&2; exit 1; }
name="drawer-$(date -u +%Y%m%dT%H%M%S%N).tar.gz"
# Stopping the writer guarantees SQLite WAL and content blobs agree.
docker compose stop drawer
restart() { docker compose up -d drawer >/dev/null; }
trap restart EXIT
docker compose run --rm --no-deps --user root --entrypoint sh drawer -c \
  'tar --exclude="./.maintenance.lock" --exclude="./users/*/.maintenance.lock" --exclude="./pre-restore-*" --exclude="./.restore-stage-*" -czf "/backups/$1" -C /data .' sh "$name"
( cd backups && sha256sum "$name" > "$name.sha256" )
printf 'Backup: %s/backups/%s\n' "$(pwd)" "$name"
printf 'Move the archive and its .sha256 file to a DIFFERENT device or cloud account.\n'

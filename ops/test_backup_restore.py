"""Exercise real maintenance scripts and tar with a mocked Docker volume boundary."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class MaintenanceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / 'ops').mkdir()
        (self.root / 'backups').mkdir()
        (self.root / 'data/blobs').mkdir(parents=True)
        (self.root / 'bin').mkdir()
        for script in ['backup.sh', 'restore.sh']:
            shutil.copy(Path(__file__).parent / script, self.root / 'ops' / script)
        (self.root / 'data/drawer.db').write_text('original database')
        (self.root / 'data/blobs/content').write_text('original blob')
        docker = self.root / 'bin/docker'
        docker.write_text('''#!/usr/bin/env python3
import os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['MAINTENANCE_ROOT'])
args = sys.argv[1:]
with (root / 'calls').open('a') as log:
    log.write(' '.join(args[:3]) + '\\n')
if args[:2] == ['compose', 'run']:
    if os.environ.get('FAIL_RESTORE') and len(args) > 1 and 'test -f' in args[args.index('-c') + 1]:
        sys.exit(1)
    i = args.index('-c')
    command = args[i + 1].replace('/data', str(root / 'data')).replace('/backups', str(root / 'backups'))
    sys.exit(subprocess.call(['sh', '-c', command, *args[i + 2:]]))
sys.exit(0)
''')
        docker.chmod(0o755)
        chown = self.root / 'bin/chown'
        chown.write_text('#!/bin/sh\nexit 0\n')
        chown.chmod(0o755)
        self.env = dict(os.environ, MAINTENANCE_ROOT=str(self.root),
                        PATH=str(self.root / 'bin') + ':' + os.environ['PATH'])

    def run_script(self, script, *args, input=''):
        return subprocess.run(['bash', str(self.root / 'ops' / script), *map(str, args)],
                              input=input, text=True, capture_output=True, env=self.env)

    def backup(self):
        result = self.run_script('backup.sh')
        self.assertEqual(result.returncode, 0, result.stderr)
        return next((self.root / 'backups').glob('*.tar.gz'))

    def test_roundtrip_preserves_safety_backup_and_handles_quoted_filename(self):
        archive = self.backup()
        renamed = archive.with_name("backup's copy.tar.gz")
        archive.rename(renamed)
        Path(str(renamed) + '.sha256').write_text(hashlib.sha256(renamed.read_bytes()).hexdigest() + '  original-name.tar.gz\n')
        (self.root / 'data/drawer.db').write_text('newer database')
        (self.root / 'data/blobs/content').write_text('newer blob')
        result = self.run_script('restore.sh', renamed, input='RESTORE\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'data/drawer.db').read_text(), 'original database')
        self.assertEqual((self.root / 'data/blobs/content').read_text(), 'original blob')
        snapshot = next((self.root / 'data').glob('pre-restore-*'))
        self.assertEqual((snapshot / 'drawer.db').read_text(), 'newer database')
        self.assertEqual(len(list((self.root / 'backups').glob('drawer-before-restore-*tar.gz'))), 1)
        self.assertFalse(list((self.root / 'data').glob('.restore-stage-*')))
        calls = (self.root / 'calls').read_text().splitlines()
        self.assertEqual(calls[-4:], ['compose stop drawer', 'compose run --rm', 'compose run --rm', 'compose up -d'])

    def test_store_lease_is_excluded_from_new_backups(self):
        import tarfile
        (self.root / 'data/.maintenance.lock').write_text('live lease')
        archive = self.backup()
        with tarfile.open(archive) as backup:
            self.assertNotIn('./.maintenance.lock', backup.getnames())
        result = self.run_script('restore.sh', archive, input='RESTORE\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / 'data/.maintenance.lock').read_text(), 'live lease')

    def test_legacy_snapshot_lease_does_not_leave_restore_stage(self):
        import tarfile
        archive = self.root / 'backups/legacy.tar.gz'
        (self.root / 'data/.maintenance.lock').write_text('old copied lease')
        with tarfile.open(archive, 'w:gz') as backup:
            backup.add(self.root / 'data', arcname='.')
        Path(str(archive) + '.sha256').write_text(hashlib.sha256(archive.read_bytes()).hexdigest())
        (self.root / 'data/.maintenance.lock').write_text('live lease')
        result = self.run_script('restore.sh', archive, input='RESTORE\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(list((self.root / 'data').glob('.restore-stage-*')))
        self.assertEqual((self.root / 'data/.maintenance.lock').read_text(), 'live lease')

    def test_checksum_must_match_selected_archive(self):
        archive = self.backup()
        archive.write_bytes(b'corrupt')
        result = self.run_script('restore.sh', archive, input='RESTORE\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Checksum mismatch', result.stderr)
        self.assertEqual((self.root / 'data/drawer.db').read_text(), 'original database')

    def test_failure_does_not_restart_writer(self):
        archive = self.backup()
        self.env['FAIL_RESTORE'] = '1'
        result = self.run_script('restore.sh', archive, input='RESTORE\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('remains stopped', result.stderr)
        self.assertEqual((self.root / 'calls').read_text().splitlines()[-1], 'compose run --rm')
        self.assertTrue(list((self.root / 'backups').glob('drawer-before-restore-*tar.gz')))

    def test_maintenance_lock_rejects_concurrent_invocation(self):
        import fcntl
        with (self.root / 'backups/.maintenance.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_script('backup.sh')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Another backup or restore', result.stderr)
        self.assertFalse((self.root / 'calls').exists())


if __name__ == '__main__':
    unittest.main()

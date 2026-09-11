#!/usr/bin/env python3
"""Library-boundary experiments, not provider admission or app-server tests.

Run with --native-probe pointing to build_probe.py's pinned native harness.
All policy, config and sentinel stores are private fixtures. No native executable,
model, auth, hook, or datastore runtime is started by this harness mode.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

PROBE = None


def snapshot(root):
    return {str(p.relative_to(root)): (p.stat().st_mode, hashlib.sha256(p.read_bytes()).hexdigest())
            if p.is_file() else (p.stat().st_mode, None)
            for p in root.rglob('*')}


class ConfigOnlyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='age356-config-only-')
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in ['home', 'codex', 'cwd/.git', 'system', 'managed', 'requested-store',
                     'redirected-store', 'xdg-config', 'xdg-data', 'tmp']:
            (self.root / name).mkdir(parents=True)
        # Deliberately invalid database bytes: recovery would change these.
        for name in ['requested-store', 'redirected-store']:
            (self.root / name / 'state_5.sqlite').write_bytes(b'private corrupt database sentinel')
        self.overrides = ['-c', 'features.hooks=true', '-c', 'features.shell_tool=false',
                          '-c', f'sqlite_home={json.dumps(str(self.root / "requested-store"))}']

    def run_loader(self):
        env = dict(PATH='/usr/bin:/bin', HOME=str(self.root / 'home'),
                   CODEX_HOME=str(self.root / 'codex'),
                   XDG_CONFIG_HOME=str(self.root / 'xdg-config'),
                   XDG_DATA_HOME=str(self.root / 'xdg-data'), TMPDIR=str(self.root / 'tmp'),
                   FIXTURE_NATIVE_ARGS=json.dumps(self.overrides))
        before = snapshot(self.root)
        result = subprocess.run([str(PROBE), str(self.root), str(self.root / 'codex'),
                                 str(self.root / 'cwd'), 'config-only'],
                                env=env, cwd=self.root / 'cwd', capture_output=True,
                                text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(snapshot(self.root), before, 'loader changed fixture files')
        return json.loads(result.stdout)

    def test_cli_settings_without_storage_startup(self):
        result = self.run_loader()
        self.assertEqual(result['effective_owned_settings']['sqlite_home'], str(self.root / 'requested-store'))
        self.assertTrue(result['effective_features']['hooks'])
        self.assertFalse(result['effective_features']['shell_tool'])

    def test_exact_managed_redirection_beats_cli_without_opening_store(self):
        redirected = self.root / 'redirected-store'
        (self.root / 'system/requirements.toml').write_text(
            f'sqlite_home={json.dumps(str(redirected))}\nallow_managed_hooks_only=true\n')
        result = self.run_loader()
        self.assertEqual(result['effective_owned_settings']['sqlite_home'], str(redirected))
        self.assertTrue(result['requirements']['allow_managed_hooks_only'])
        # Unlike fake echo tests, the real native requirements application changed
        # the requested path. The preflight could reject here, before DB code.

    def test_system_config_and_managed_feature_requirements_are_visible(self):
        prompt = self.root / 'system/instructions.md'
        prompt.write_text('fixture-only instructions')
        (self.root / 'system/config.toml').write_text(
            f'model_instructions_file={json.dumps(str(prompt))}\n')
        (self.root / 'system/requirements.toml').write_text('[features]\nhooks=false\n')
        result = self.run_loader()
        self.assertEqual(result['effective_owned_settings']['model_instructions_file'], str(prompt))
        # Feature requirements are distinct from typed CLI config, just as in
        # production admission; checking the effective TOML alone is insufficient.
        self.assertTrue(result['effective_features']['hooks'])
        self.assertFalse(result['requirements']['features']['hooks'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--native-probe', required=True, type=Path)
    args, rest = parser.parse_known_args()
    PROBE = args.native_probe.resolve(strict=True)
    unittest.main(argv=[__file__, *rest])

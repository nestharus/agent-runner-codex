"""Exercise label migration and rollback backups with the built provider."""
import argparse
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

REPO = Path(__file__).resolve().parents[1]
PROVIDER = REPO / 'target/debug/agent-runner-codex'
ACCOUNTS = ['codex', 'codex2', 'codex3', 'codex4', 'codex5']


class LabelInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='codex-label-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.config = self.root / 'config'
        self.models = self.config / 'models'
        self.models.mkdir(parents=True)
        self.original_providers = ''.join(
            f'[{account}]\ncommand="{account}"\ninteractive_args=["--dangerously-bypass-approvals-and-sandbox"]\n'
            'system_prompt_override="Keep the account instructions."\n'
            f'[{account}.tool_restrictions]\nkind="codex"\n'
            f'[{account}.resume]\nkind="subcommand"\nsubcommand=["resume"]\n'
            f'[{account}.implementation]\nfamily="codex"\nexecutable="/old/provider"\n'
            for account in ACCOUNTS
        )
        (self.config / 'providers.toml').write_text(self.original_providers)
        for effort in ['low', 'max']:
            (self.models / f'gpt-luna-{effort}.toml').write_text('# previous OpenCode route\n')
        self.untouched = self.models / 'gpt-high.toml'
        self.untouched.write_text('# existing Astra route\n')

    def install(self, *extra, provider=None):
        return subprocess.run([
            sys.executable, str(REPO / 'scripts/install-labels.py'),
            '--luna-labels', '--config-root', str(self.config),
            '--stage-root', str(self.root / 'stage'), '--provider-path', str(provider or PROVIDER),
            *extra,
        ], capture_output=True, text=True, timeout=20)

    def test_luna_apply_preserves_other_labels_and_backs_up_replaced_routes(self):
        result = self.install('--apply')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.untouched.read_text(), '# existing Astra route\n')
        self.assertEqual(sorted(p.stem for p in self.models.glob('*.toml')),
                         ['gpt-high', 'gpt-luna-low', 'gpt-luna-max'])
        for effort in ['low', 'max']:
            route = tomllib.loads((self.models / f'gpt-luna-{effort}.toml').read_text())
            self.assertEqual(route['provider']['path'], str(PROVIDER))
            self.assertEqual([p['name'] for p in route['providers']], ACCOUNTS)
            for account in route['providers']:
                expected = ['-m', 'gpt-5.6-luna', '-c', f'model_reasoning_effort="{effort}"']
                self.assertEqual(account['args'], expected)
                self.assertEqual(account['interactive_args'], expected)
        accounts = tomllib.loads((self.config / 'providers.toml').read_text())
        for name in ACCOUNTS:
            account = accounts[name]
            self.assertEqual(account['command'], str(PROVIDER))
            self.assertEqual(account['interactive_args'], ['interactive', '--settings-id', name,
                                                         '--config-root', str(self.config)])
            self.assertEqual(account['resume'], {'kind':'flag', 'flag':'--resume'})
            self.assertEqual(account['system_prompt_override'], 'Keep the account instructions.')
            self.assertEqual(account['tool_restrictions'], {'kind':'codex'})
        backups = list((self.config / 'backups').glob('codex-luna-*'))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / 'providers.toml').read_text(), self.original_providers)
        for effort in ['low', 'max']:
            self.assertEqual((backups[0] / 'models' / f'gpt-luna-{effort}.toml').read_text(),
                             '# previous OpenCode route\n')

    def test_missing_luna_discovery_rejects_apply_without_mutation(self):
        old = self.root / 'old-provider'
        old.write_text('#!/usr/bin/env python3\nimport json,sys\n'
                       'r=json.load(sys.stdin)\n'
                       'print(json.dumps(dict(contract=r["contract"], request_id=r["request_id"], '
                       'ok=True, result=dict(models=[]))))\n')
        old.chmod(0o755)
        result = self.install('--apply', provider=old)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('must advertise exactly one gpt-luna-low route', result.stderr)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)
        for effort in ['low', 'max']:
            self.assertEqual((self.models / f'gpt-luna-{effort}.toml').read_text(),
                             '# previous OpenCode route\n')
        self.assertFalse((self.config / 'backups').exists())

    def test_staging_changes_pty_route_without_mutating_installed_configuration(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)
        staged = tomllib.loads((self.root / 'stage/providers.toml.proposed').read_text())
        self.assertEqual(staged['codex3']['resume'], {'kind':'flag', 'flag':'--resume'})
        self.assertEqual(staged['codex3']['interactive_args'][0], 'interactive')

    def test_old_headless_provider_cannot_activate_pty_routes(self):
        old = self.root / 'headless-only-provider'
        old.write_text('#!/usr/bin/env python3\nimport os,sys\n'
                       'if sys.argv[1:] == ["interactive", "--help"]: sys.exit(1)\n'
                       f'os.execv({str(PROVIDER)!r}, [{str(PROVIDER)!r}, *sys.argv[1:]])\n')
        old.chmod(0o755)
        result = self.install('--apply', provider=old)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Managed Codex PTY launcher is unavailable', result.stderr)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)
        self.assertFalse((self.config / 'backups').exists())


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=PROVIDER)
    args, remaining = parser.parse_known_args()
    PROVIDER = args.binary.resolve()
    unittest.main(argv=[sys.argv[0], *remaining])

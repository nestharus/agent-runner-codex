"""Exercise label migration and rollback backups with the built provider."""
import argparse
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest

REPO = Path(__file__).resolve().parents[1]
PROVIDER = REPO / 'target/debug/agent-runner-codex'
ACCOUNTS = ['codex', 'codex2', 'codex3', 'codex4', 'codex5']
EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']


class LabelInstallerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='codex-label-test-')
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(self.preserve_evidence)
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
        (self.models / 'codex-gpt-high.toml').write_text('# existing temporary Astra route\n')
        (self.models / 'codex-exec-bench.toml').write_text('# isolated benchmark sentinel\n')

    def preserve_evidence(self):
        destination = os.environ.get('CODEX_TEST_EVIDENCE_DIR')
        if destination:
            shutil.copytree(self.root, Path(destination) / self.id(), symlinks=True)

    def install(self, *extra, provider=None, family='luna'):
        return subprocess.run([
            sys.executable, str(REPO / 'scripts/install-labels.py'),
            f'--{family}-labels', '--config-root', str(self.config),
            '--stage-root', str(self.root / 'stage'), '--provider-path', str(provider or PROVIDER),
            *extra,
        ], capture_output=True, text=True, timeout=20)

    def standard_fixture(self):
        for effort in EFFORTS:
            route = ('# Preserve standard route formatting and comments.\n'
                     f'provider = {{ path = "{PROVIDER}" }}\n'
                     '[[inputs]]\ndefault_input = true\ndescription = "The text prompt"\n'
                     'name = "prompt"\nrequired = true\ntype = "string"\n')
            args = json.dumps(["-m", "gpt-6-astra", "-c", f'model_reasoning_effort="{effort}"'])
            route += ''.join(
                f'\n[[providers]]\nargs = {args}\ninteractive_args = {args}\n'
                f'name = "{account}"\n' for account in ACCOUNTS)
            (self.models / f'gpt-{effort}.toml').write_text(route)
        (self.models / 'gpt-sol-high.toml').write_text('# unrelated Sol sentinel\n')
        return {p.name: p.read_bytes() for p in self.models.iterdir()}

    def assert_standard_routes(self, directory):
        for label in EFFORTS:
            route = tomllib.loads((directory / f'gpt-{label}.toml').read_text())
            effort = 'low' if label == 'low' else 'medium'
            expected = ['-m', 'gpt-6-astra', '-c', f'model_reasoning_effort="{effort}"']
            self.assertEqual([p['name'] for p in route['providers']], ACCOUNTS)
            for account in route['providers']:
                self.assertEqual(account['args'], expected, label)
                self.assertEqual(account['interactive_args'], expected, label)

    def test_standard_staging_maps_only_affected_efforts_without_mutation(self):
        before = self.standard_fixture()
        result = self.install(family='standard')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_standard_routes(self.root / 'stage/models')
        self.assertEqual({p.name: p.read_bytes() for p in self.models.iterdir()}, before)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)

    def test_standard_apply_backs_up_affected_routes_preserves_unchanged_bytes(self):
        before = self.standard_fixture()
        result = self.install('--apply', family='standard')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_standard_routes(self.models)
        affected = {'gpt-high.toml', 'gpt-xhigh.toml', 'gpt-max.toml'}
        for name, content in before.items():
            if name not in affected:
                self.assertEqual((self.models / name).read_bytes(), content, name)
        self.assertEqual(set(p.name for p in self.models.iterdir()), set(before))
        backups = list((self.config / 'backups').glob('codex-astra-*'))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / 'providers.toml').read_text(), self.original_providers)
        for name in affected:
            self.assertEqual((backups[0] / 'models' / name).read_bytes(), before[name])
        applied = {p.name: p.read_bytes() for p in self.models.iterdir()}
        again = self.install('--apply', family='standard')
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertEqual({p.name: p.read_bytes() for p in self.models.iterdir()}, applied)

    def test_standard_stale_discovery_rejects_apply_without_mutation(self):
        before = self.standard_fixture()
        old = self.root / 'stale-provider'
        old.write_text('#!/usr/bin/env python3\nimport json,subprocess,sys\n'
                       f'r=subprocess.run([{str(PROVIDER)!r}, *sys.argv[1:]], input=sys.stdin.read(), capture_output=True, text=True)\n'
                       'v=json.loads(r.stdout)\n'
                       'for e in v["result"]["models"]:\n'
                       ' if e["name"] in ["gpt-high","gpt-xhigh","gpt-max"]: e["provider_args"][3]="model_reasoning_effort="+json.dumps(e["name"].split("-")[-1])\n'
                       'print(json.dumps(v))\n')
        old.chmod(0o755)
        result = self.install('--apply', family='standard', provider=old)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("model, arguments, or eligible accounts do not match", result.stderr)
        self.assertEqual({p.name: p.read_bytes() for p in self.models.iterdir()}, before)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)
        self.assertFalse((self.config / 'backups').exists())

    def test_checked_in_standard_and_compatibility_examples(self):
        for prefix in ['gpt', 'codex-gpt']:
            for label in EFFORTS:
                route = tomllib.loads((REPO / f'examples/models/{prefix}-{label}.toml').read_text())
                effort = ('low' if label == 'low' else 'medium') if prefix == 'gpt' else label
                expected = ['-m', 'gpt-6-astra', '-c', f'model_reasoning_effort="{effort}"']
                for account in route['providers']:
                    self.assertEqual(account['args'], expected)
                    self.assertEqual(account['interactive_args'], expected)

    def test_luna_apply_preserves_other_labels_and_backs_up_replaced_routes(self):
        self.assert_family_apply('luna')

    def test_terra_apply_preserves_other_labels_and_backs_up_replaced_routes(self):
        for effort in ['low', 'max']:
            (self.models / f'gpt-terra-{effort}.toml').write_text('# previous OpenCode route\n')
        self.assert_family_apply('terra')

    def test_sol_apply_preserves_other_labels_and_backs_up_replaced_routes(self):
        for effort in ['low', 'max']:
            (self.models / f'gpt-sol-{effort}.toml').write_text('# previous OpenCode route\n')
        self.assert_family_apply('sol')

    def assert_family_apply(self, family):
        untouched = {path.name: path.read_text() for path in self.models.glob('*.toml')
                     if not path.name.startswith(f'gpt-{family}-')}
        result = self.install('--apply', family=family)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for filename, content in untouched.items():
            self.assertEqual((self.models / filename).read_text(), content)
        self.assertEqual(sorted(p.name for p in self.models.glob('*.toml')),
                         sorted([*untouched, *(f'gpt-{family}-{effort}.toml' for effort in EFFORTS)]))
        for effort in EFFORTS:
            route = tomllib.loads((self.models / f'gpt-{family}-{effort}.toml').read_text())
            self.assertEqual(route['provider']['path'], str(PROVIDER))
            self.assertEqual([p['name'] for p in route['providers']], ACCOUNTS)
            for account in route['providers']:
                expected = ['-m', f'gpt-5.6-{family}', '-c', f'model_reasoning_effort="{effort}"']
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
        backups = list((self.config / 'backups').glob(f'codex-{family}-*'))
        self.assertEqual(len(backups), 1)
        self.assertEqual((backups[0] / 'providers.toml').read_text(), self.original_providers)
        for effort in ['low', 'max']:
            self.assertEqual((backups[0] / 'models' / f'gpt-{family}-{effort}.toml').read_text(),
                             '# previous OpenCode route\n')
        self.assertEqual(sorted(p.name for p in (backups[0] / 'models').iterdir()),
                         [f'gpt-{family}-low.toml', f'gpt-{family}-max.toml'])

    def test_missing_new_luna_route_rejects_apply_without_mutation(self):
        self.assert_missing_route('luna', 'medium')

    def test_missing_terra_route_rejects_apply_without_mutation(self):
        self.assert_missing_route('terra', 'high')

    def test_missing_sol_route_rejects_apply_without_mutation(self):
        self.assert_missing_route('sol', 'xhigh')

    def assert_missing_route(self, family, effort):
        before = {path.name: path.read_text() for path in self.models.glob('*.toml')}
        old = self.root / 'incomplete-provider'
        missing = f'gpt-{family}-{effort}'
        old.write_text('#!/usr/bin/env python3\nimport json,subprocess,sys\n'
                       f'result=subprocess.run([{str(PROVIDER)!r}, *sys.argv[1:]], input=sys.stdin.read(), capture_output=True, text=True)\n'
                       'response=json.loads(result.stdout)\n'
                       f'response["result"]["models"]=[entry for entry in response["result"]["models"] if entry["name"] != {missing!r}]\n'
                       'print(json.dumps(response))\n')
        old.chmod(0o755)
        result = self.install('--apply', provider=old, family=family)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f'must advertise exactly one {missing} route', result.stderr)
        self.assertEqual((self.config / 'providers.toml').read_text(), self.original_providers)
        self.assertEqual({path.name: path.read_text() for path in self.models.glob('*.toml')}, before)
        self.assertFalse((self.config / 'backups').exists())

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

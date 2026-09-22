#!/usr/bin/env python3
"""Offline real CLI/installer checks with idle, open stdin."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
BINARY = None


class IdleInputTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='codex-install-idle-')
        self.root = Path(self.temporary.name)
        self.counter = 0

    def tearDown(self):
        if destination := os.environ.get('CODEX_TEST_EVIDENCE_DIR'):
            shutil.copytree(self.root, Path(destination) / self.root.name)
        self.temporary.cleanup()

    def idle_run(self, command, timeout=4):
        # wait(), unlike communicate(), does not close the pipe writer. Output
        # goes to files so the idle-input check cannot block on output backpressure.
        self.counter += 1
        record = self.root / f'run-{self.counter}'
        record.mkdir()
        (record / 'command.json').write_text(json.dumps(command))
        with (record / 'stdout.raw').open('wb') as stdout, (record / 'stderr.raw').open('wb') as stderr:
            child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr,
                                     start_new_session=True, env={**os.environ, 'HOME': str(self.root / 'home')})
            timed_out = False
            try:
                code = child.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                os.killpg(child.pid, signal.SIGKILL)
                code = child.wait()
            finally:
                child.stdin.close()
        (record / 'result.json').write_text(json.dumps({'exit_status': code, 'timed_out_with_stdin_open': timed_out}))
        self.assertFalse(timed_out, f'waited for idle stdin: {command}')
        return code, (record / 'stdout.raw').read_text(), (record / 'stderr.raw').read_text()

    def installer_command(self, binary, script=None):
        prompt = self.root / 'prompt.md'
        prompt.write_text('isolated installer test prompt\n')
        dependency = self.root / 'fake-dependency'
        dependency.write_text('#!/bin/sh\nexit 0\n')
        dependency.chmod(0o755)
        return ['python3', str(script or REPO / 'scripts/install-provider.py'), '--binary', str(binary),
                '--config-root', str(self.root / 'config'), '--bin-dir', str(self.root / 'bin'),
                '--system-prompt-file', str(prompt),
                *[part for key in ['--codex-bin', '--bun-bin', '--agent-bash-bin', '--agent-runner-bin']
                  for part in [key, str(dependency)]]]

    def test_real_version_ignores_idle_open_stdin(self):
        code, stdout, stderr = self.idle_run([str(BINARY), '--version'])
        self.assertEqual((code, stdout, stderr), (0, 'agent-runner-codex 0.1.0\n', ''))

    def test_real_provider_installs_from_idle_open_stdin(self):
        self.assertFalse((self.root / 'home/.config/opencode/tools/bash.ts').exists())
        code, stdout, stderr = self.idle_run(self.installer_command(BINARY))
        self.assertEqual(code, 0, stdout + stderr)
        installed = self.root / 'config/agent-runner-codex/agent-runner-codex'
        self.assertEqual(installed.read_bytes(), BINARY.read_bytes())
        self.assertEqual((self.root / 'bin/agent-runner-codex').resolve(), installed)
        integrations = self.root / 'config/agent-runner-codex/integrations'
        expected = REPO / 'integrations/opencode/tools/bash.ts'
        self.assertEqual((integrations / 'opencode/tools/bash.ts').read_bytes(), expected.read_bytes())
        self.assertEqual(hashlib.sha256(expected.read_bytes()).hexdigest(),
                         '64e82c7a8677122155d7e6a9955fa87dd8d31cc491b8d922a178b250c2e47bc8')
        self.assertIn('../opencode/tools/bash.ts',
                      (integrations / 'codex/agent-bash-mcp.ts').read_text())
        identity = json.loads(subprocess.check_output([str(installed), '--integration-hashes'], text=True))
        self.assertEqual(identity['assets']['opencode/tools/bash.ts'],
                         hashlib.sha256(expected.read_bytes()).hexdigest())
        bun = shutil.which('bun')
        self.assertIsNotNone(bun)
        requests = [
            {'jsonrpc': '2.0', 'id': 1, 'method': 'initialize',
             'params': {'protocolVersion': '2024-11-05', 'capabilities': {},
                        'clientInfo': {'name': 'install-fixture', 'version': '1'}}},
            {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'},
        ]
        run = subprocess.run([bun, '--no-install', str(integrations / 'codex/agent-bash-mcp.ts')],
                             input=''.join(json.dumps(request) + '\n' for request in requests),
                             text=True, capture_output=True, timeout=5,
                             env={**os.environ, 'HOME': str(self.root / 'home'),
                                  'AGENT_BASH_BIN': str(self.root / 'fake-dependency'),
                                  'AGENT_BASH_AGENT_RUNNER_BIN': str(self.root / 'fake-dependency')})
        self.assertEqual(run.returncode, 0, run.stderr)
        replies = [json.loads(line) for line in run.stdout.splitlines()]
        tools = next(reply['result']['tools'] for reply in replies if reply.get('id') == 2)
        self.assertEqual([tool['name'] for tool in tools], ['bash'])
        self.assertEqual(set(tools[0]['inputSchema']['properties']),
                         {'command', 'handle', 'delivery', 'workdir'})

    def test_tampered_internal_bundle_rejected_before_mutation(self):
        for changed in ('bash', 'manifest'):
            with self.subTest(changed=changed):
                repo = self.root / changed
                (repo / 'scripts').mkdir(parents=True)
                shutil.copy2(REPO / 'scripts/install-provider.py', repo / 'scripts/install-provider.py')
                shutil.copytree(REPO / 'integrations', repo / 'integrations')
                bash = repo / 'integrations/opencode/tools/bash.ts'
                manifest = repo / 'integrations/opencode/BASH_SOURCE.json'
                if changed == 'bash':
                    bash.write_bytes(bash.read_bytes() + b'\n// tampered\n')
                else:
                    data = json.loads(manifest.read_text())
                    data['source_commit'] = '0' * 40
                    manifest.write_text(json.dumps(data))
                code, _, stderr = self.idle_run(self.installer_command(BINARY, repo / 'scripts/install-provider.py'))
                self.assertNotEqual(code, 0)
                self.assertIn('Codex Bash', stderr)
                self.assertFalse((self.root / 'config').exists())
                self.assertFalse((self.root / 'bin').exists())

    def test_unrecognized_installed_bash_rejected_without_replacement(self):
        installed = self.root / 'config/agent-runner-codex/integrations/opencode/tools/bash.ts'
        installed.parent.mkdir(parents=True)
        installed.write_text('newer Bash candidate\n')
        code, _, stderr = self.idle_run(self.installer_command(BINARY))
        self.assertNotEqual(code, 0)
        self.assertIn('unrecognized SHA-256', stderr)
        self.assertEqual(installed.read_text(), 'newer Bash candidate\n')
        self.assertFalse((self.root / 'bin').exists())

    def test_binary_with_different_embedded_bash_rejected_before_mutation(self):
        identity = json.loads(subprocess.check_output([str(BINARY), '--integration-hashes'], text=True))
        identity['assets']['opencode/tools/bash.ts'] = '0' * 64
        binary = self.root / 'mismatched-provider'
        binary.write_text('#!/usr/bin/env python3\nimport sys\n'
                          'if sys.argv[1:] == ["--version"]: print("agent-runner-codex fixture")\n'
                          'elif sys.argv[1:] == ["--integration-hashes"]: print('
                          + repr(json.dumps(identity)) + ')\nelse: sys.exit(1)\n')
        binary.chmod(0o755)
        code, _, stderr = self.idle_run(self.installer_command(binary))
        self.assertNotEqual(code, 0)
        self.assertIn('embedded integration differs', stderr)
        self.assertFalse((self.root / 'config').exists())
        self.assertFalse((self.root / 'bin').exists())

    def test_failed_reinstall_restores_previous_package_from_backup(self):
        command = self.installer_command(BINARY)
        code, stdout, stderr = self.idle_run(command)
        self.assertEqual(code, 0, stdout + stderr)
        install = self.root / 'config/agent-runner-codex'
        prior_binary = (install / 'agent-runner-codex').read_bytes()
        prior_bash = (install / 'integrations/opencode/tools/bash.ts').read_bytes()
        config = install / 'config.toml'
        config.unlink()
        config.mkdir()
        (config / 'sentinel').write_text('previous configuration directory\n')
        code, _, stderr = self.idle_run(command)
        self.assertNotEqual(code, 0)
        self.assertIn('previous files restored', stderr)
        self.assertEqual((install / 'agent-runner-codex').read_bytes(), prior_binary)
        self.assertEqual((install / 'integrations/opencode/tools/bash.ts').read_bytes(), prior_bash)
        self.assertEqual((config / 'sentinel').read_text(), 'previous configuration directory\n')
        self.assertEqual((self.root / 'bin/agent-runner-codex').resolve(), install / 'agent-runner-codex')
        backups = list((self.root / 'config/backups').iterdir())
        self.assertEqual(len(backups), 2)
        self.assertTrue(any((backup / 'config.toml/sentinel').exists() for backup in backups))

    def test_installer_probe_explicitly_supplies_eof(self):
        # Unlike the corrected real CLI, this executable requires EOF. This
        # independent control detects an installer that merely inherits stdin.
        binary = self.root / 'eof-version'
        identity = subprocess.check_output([str(BINARY), '--integration-hashes'], text=True)
        binary.write_text('#!/usr/bin/env python3\nimport sys\nassert sys.stdin.read() == ""\n'
                          'if sys.argv[1:] == ["--version"]: print("agent-runner-codex fixture")\n'
                          'elif sys.argv[1:] == ["--integration-hashes"]: print(' + repr(identity) + ')\n'
                          'else: sys.exit(1)\n')
        binary.chmod(0o755)
        code, stdout, stderr = self.idle_run(self.installer_command(binary))
        self.assertEqual(code, 0, stdout + stderr)

    def test_invalid_version_rejects_before_install_mutations(self):
        binary = self.root / 'wrong-version'
        binary.write_text('#!/bin/sh\nprintf "not-the-provider\\n"\n')
        binary.chmod(0o755)
        code, stdout, stderr = self.idle_run(self.installer_command(binary))
        self.assertNotEqual(code, 0)
        self.assertIn('did not identify itself', stderr)
        self.assertFalse((self.root / 'config').exists())
        self.assertFalse((self.root / 'bin').exists())


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, required=True)
    args, remaining = parser.parse_known_args()
    BINARY = args.binary.resolve()
    unittest.main(argv=[__file__, *remaining])

#!/usr/bin/env python3
"""Offline real CLI/installer checks with idle, open stdin (AGE-342 F2)."""
import argparse
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
        # goes to files so the F2 oracle cannot accidentally exercise F1 instead.
        self.counter += 1
        record = self.root / f'run-{self.counter}'
        record.mkdir()
        (record / 'command.json').write_text(json.dumps(command))
        with (record / 'stdout.raw').open('wb') as stdout, (record / 'stderr.raw').open('wb') as stderr:
            child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr, start_new_session=True)
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

    def installer_command(self, binary):
        prompt = self.root / 'prompt.md'
        prompt.write_text('isolated installer test prompt\n')
        dependency = self.root / 'fake-dependency'
        dependency.write_text('#!/bin/sh\nexit 0\n')
        dependency.chmod(0o755)
        return ['python3', str(REPO / 'scripts/install-provider.py'), '--binary', str(binary),
                '--config-root', str(self.root / 'config'), '--bin-dir', str(self.root / 'bin'),
                '--opencode-bash', str(REPO / 'integrations/opencode/tools/bash.ts'),
                '--system-prompt-file', str(prompt),
                *[part for key in ['--codex-bin', '--bun-bin', '--agent-bash-bin', '--agent-runner-bin']
                  for part in [key, str(dependency)]]]

    def test_real_version_ignores_idle_open_stdin(self):
        code, stdout, stderr = self.idle_run([str(BINARY), '--version'])
        self.assertEqual((code, stdout, stderr), (0, 'agent-runner-codex 0.1.0\n', ''))

    def test_real_provider_installs_from_idle_open_stdin(self):
        code, stdout, stderr = self.idle_run(self.installer_command(BINARY))
        self.assertEqual(code, 0, stdout + stderr)
        installed = self.root / 'config/agent-runner-codex/agent-runner-codex'
        self.assertEqual(installed.read_bytes(), BINARY.read_bytes())
        self.assertEqual((self.root / 'bin/agent-runner-codex').resolve(), installed)

    def test_installer_probe_explicitly_supplies_eof(self):
        # Unlike the corrected real CLI, this executable requires EOF. This
        # independent control detects an installer that merely inherits stdin.
        binary = self.root / 'eof-version'
        binary.write_text('#!/usr/bin/env python3\nimport sys\nassert sys.argv[1:] == ["--version"]\nassert sys.stdin.read() == ""\nprint("agent-runner-codex fixture")\n')
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

#!/usr/bin/env python3
"""Synthetic checks for the inventory producer, not a native Codex host test.

Run only inside an externally established private user/net/mount/PID namespace
with production /home, /root, /run and /tmp masked, env-i/private HOME/XDG/CODEX
state, read-only source/dependency inputs, DAC override/readsearch dropped and
PID1 reaping. Enable only private loopback after the boundary probe.

The full ten-test invocation requires CODEX_INVENTORY_TEST_BUN to name an
absolute private Bun executable and CODEX_INVENTORY_TEST_PRIVATE_ROOT to name
the private harness root. That root must contain the actual boundary.json probe
report: masks, read_only_inputs and DAC_denied must be true, and namespaces must
record the current user/net/mnt/pid identities from /proc/self/ns. The native producer requires --boundary-report and the four
InventoryProducerFailureTest controls check those recorded identities and create
failure-cases/<test-name digest> without exist_ok; use fresh directories and
short paths for Unix sockets on every run. Do not fabricate or reuse a report
from another namespace. These checks do not establish isolation themselves.

Inside that prepared environment, python3 tests/test_tui_inventory_producer.py -v
selects all ten tests. The original six InventoryProducerTest tests exercise the
bridge/oracle; the four additional controls run the actual producer PTY/HTTP
path with fake provider/native executables. A Bun-only host command is not a
complete safe entry. See scripts/README.md for the retained harness contract.
"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import uuid

import verify_codex_tui_inventory as inventory

REPO = Path(__file__).resolve().parents[1]


class InventoryProducerTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='inventory-producer-')
        self.root = Path(self.tmp.name)
        # Deliberately use the standalone producer setup, not AdapterTest.setUp.
        with patch.dict(os.environ, {'OPENAI_API_KEY': 'synthetic-poison',
                                     'AGENT_BASH_BIN': '/must-not-run',
                                     'OULIPOLY_PARENT_INVOCATION': 'stale'}):
            self.env, self.spooler, self.runner, self.prompt = inventory.fixture_environment(self.root)
        self.process = None
        self.receiver = None

    def tearDown(self):
        if self.process:
            self.process.stdin.close()
            self.process.wait()
            self.process.stdout.close()
            self.process.stderr.close()
        if self.receiver: self.receiver.close()
        self.tmp.cleanup()

    def result(self, receipt_failure=False):
        bun = inventory.explicit_executable(os.environ['CODEX_INVENTORY_TEST_BUN'])
        self.env.update(AGENT_RUNNER_CODEX_INTERACTIVE='1',
                        AGENT_RUNNER_CODEX_SESSION_BINDING='tool_metadata')
        if receipt_failure:
            self.env['FAKE_RECEIPT_FAILURE'] = '1'
        self.process = subprocess.Popen(
            [str(bun), '--no-install', str(REPO/'integrations/codex/agent-bash-mcp.ts')],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, env=self.env, cwd=self.root)
        request = {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call', 'params': {
            'name': 'bash', 'arguments': {'command': 'printf inventory-tool-call'},
            '_meta': {'threadId': 'cccccccc-cccc-4ccc-8ccc-cccccccccccc'}}}
        self.process.stdin.write(json.dumps(request)+'\n'); self.process.stdin.flush()
        self.assertTrue(select.select([self.process.stdout], [], [], 5)[0], 'synthetic MCP response missing')
        response = json.loads(self.process.stdout.readline())
        self.assertNotIn('isError', response['result'])
        calls = [json.loads(line) for line in Path(self.env['FAKE_LOG']).read_text().splitlines()]
        inputs = [{'type': 'function_call_output', 'call_id': 'inventory-bash-call',
                   'output': json.dumps(response['result'])}]
        self.assertTrue(all(call['owner'] == request['params']['_meta']['threadId'] for call in calls))
        return calls, inputs, response['result']['content'][0]['text']

    def test_standalone_setup_is_fail_closed_and_private(self):
        for key in ['OPENAI_API_KEY', 'OULIPOLY_PARENT_INVOCATION', 'FAKE_RUNNING', 'FAKE_RECEIPT_FAILURE']:
            self.assertNotIn(key, self.env)
        self.assertEqual(self.env['PATH'], '/usr/bin:/bin')
        for key in ['HOME', 'CODEX_HOME', 'TMPDIR', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
                    'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR']:
            path = Path(self.env[key]); self.assertTrue(path.is_relative_to(self.root))
            self.assertEqual(path.stat().st_mode & 0o777, 0o700)
        for path in [self.spooler, self.runner, self.prompt]:
            self.assertTrue(path.is_absolute() and path.is_relative_to(self.root))
        denied = subprocess.run([str(self.runner), 'unexpected'], env=self.env, capture_output=True)
        self.assertEqual(denied.returncode, 97)
        self.assertEqual(str(uuid.UUID(inventory.INVOCATION_UUID)), inventory.INVOCATION_UUID)
        with self.assertRaises(argparse.ArgumentTypeError): inventory.explicit_executable('codex')
        with self.assertRaises(argparse.ArgumentTypeError): inventory.explicit_executable('/does-not-exist')

    def test_actual_synthetic_result_and_supported_host_encodings(self):
        calls, inputs, text = self.result()
        inventory.assert_retained_result(calls, inputs)
        for encoding in [text, {'content': [{'type': 'text', 'text': text}]},
                         [{'type': 'text', 'text': text}]]:
            with self.subTest(encoding=type(encoding).__name__):
                altered = copy.deepcopy(inputs); altered[0]['output'] = encoding
                inventory.assert_retained_result(calls, altered)

    def test_oracle_rejects_stale_order_hash_and_host_body_controls(self):
        calls, inputs, text = self.result()
        inventory.assert_retained_result(calls, inputs)
        def rejects(changed_calls, changed_inputs):
            with self.assertRaises((AssertionError, ValueError, KeyError)):
                inventory.assert_retained_result(changed_calls, changed_inputs)
        changed = copy.deepcopy(calls)
        changed[next(n for n,c in enumerate(changed) if c['args'][0]=='snapshot')]['args'][0]='consume'
        rejects(changed, inputs)
        changed = copy.deepcopy(calls)
        a,b = [next(n for n,c in enumerate(changed) if c['args'][0]==op) for op in ['snapshot','accept-output']]
        changed[a],changed[b] = changed[b],changed[a]; rejects(changed, inputs)
        changed = copy.deepcopy(calls)
        receipt = next(c for c in changed if c['args'][0]=='accept-output')
        snapshot = json.loads(receipt['args'][3]); snapshot['sha256']='0'*64
        receipt['args'][3]=json.dumps(snapshot); rejects(changed, inputs)
        changed = copy.deepcopy(calls)
        progress = next(c for c in changed if c['args'][0]=='status' and '--observe-only' not in c['args'])
        changed.remove(progress); changed.insert(0,progress); rejects(changed,inputs)
        for corrupted in [text.replace('remote ACK: unconfirmed', 'remote ACK: confirmed'),
                          text.replace('physical drain: unconfirmed', 'physical drain: confirmed'),
                          text+'fixture-output\n', text.replace('fixture-output\n',''),
                          text.replace('local receipt: durable bounded snapshot','local receipt: unconfirmed')]:
            changed = copy.deepcopy(inputs); changed[0]['output']=corrupted; rejects(calls,changed)
        changed = copy.deepcopy(inputs); changed[0]['call_id']='unrelated'; rejects(calls,changed)

    def test_existing_identity_ack_contract_allows_helper_and_mcp_reports(self):
        self.receiver = inventory.BindingReceiver(self.root/'binding.sock')
        self.env.update(OULIPOLY_LIVE_SESSION_BIND_SOCKET=str(self.root/'binding.sock'),
                        OULIPOLY_LIVE_SESSION_BIND_TOKEN='local-inventory-token',
                        OULIPOLY_PARENT_INVOCATION=json.dumps({'id': inventory.INVOCATION_UUID}),
                        AGENT_RUNNER_CODEX_REGISTRATION_CWD=str(self.root))
        bun = inventory.explicit_executable(os.environ['CODEX_INVENTORY_TEST_BUN'])
        helper = REPO/'integrations/codex/session-registration.ts'
        code = ('import {registerSession} from '+json.dumps(str(helper))+'; '
                'await registerSession("cccccccc-cccc-4ccc-8ccc-cccccccccccc",'
                +json.dumps(str(self.root))+');')
        for _ in range(2):
            result = subprocess.run([str(bun),'--no-install','-e',code],env=self.env,cwd=self.root,
                                    capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
        calls, inputs, _ = self.result()
        inventory.assert_retained_result(calls, inputs)
        # Two explicit helper calls, then bridge registration and the shared
        # adapter's own live binding; helper retries may add identical reports.
        self.assertGreaterEqual(len(self.receiver.reports),4)
        self.assertTrue(all(report == self.receiver.reports[0] for report in self.receiver.reports))

    def test_receiver_rejects_identity_drift_and_bad_token(self):
        path = self.root/'binding.sock'
        self.receiver = inventory.BindingReceiver(path)
        report = {'schema_version':1,'token':'local-inventory-token',
                  'invocation_uuid':inventory.INVOCATION_UUID,
                  'provider_session_id':'cccccccc-cccc-4ccc-8ccc-cccccccccccc'}
        def exchange(value):
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(path)); client.sendall((json.dumps(value)+'\n').encode())
                with client.makefile('rb') as stream: return json.loads(stream.readline())
        ack = exchange(report)
        self.assertEqual(ack, {'ok':True,'session_id':report['provider_session_id'],
                             'provider_session_id':report['provider_session_id'],
                             'agent_runner_invocation_id':inventory.INVOCATION_UUID})
        for field,value in [('token','wrong'),('invocation_uuid','invalid'),
                            ('provider_session_id','dddddddd-dddd-4ddd-8ddd-dddddddddddd')]:
            bad = dict(report); bad[field]=value
            self.assertEqual(exchange(bad),{'ok':False})
        self.assertEqual(exchange(report),ack)

    def test_receipt_failure_retains_bytes_but_is_not_inventory_success(self):
        calls, inputs, text = self.result(receipt_failure=True)
        self.assertTrue(text.endswith('\n--- output ---\nfixture-output\n'))
        self.assertIn('local receipt: unconfirmed; remote ACK: unconfirmed; physical drain: unconfirmed',text)
        self.assertIn('progression: unconfirmed',text)
        with self.assertRaises(AssertionError): inventory.assert_retained_result(calls,inputs)


class InventoryProducerFailureTest(unittest.TestCase):
    """Real producer PTY/HTTP path, exclusively fake provider/native executables."""
    def setUp(self):
        # Fail closed outside the caller's proven private namespace harness.
        private = Path(os.environ['CODEX_INVENTORY_TEST_PRIVATE_ROOT'])
        inventory.require_private_boundary(private/'boundary.json')
        self.private = private
        self.root = private/'failure-cases'/hashlib.sha256(self._testMethodName.encode()).hexdigest()[:12]
        self.root.mkdir(parents=True)
        (self.root/'test-id.txt').write_text(self.id()+'\n')

    def run_failure(self, request_count, validation_failure=False):
        output = self.root/'artifacts'
        native = self.root/'fake-native'
        provider = self.root/'fake-provider'
        denied = self.root/'must-not-execute'
        denied.write_text('#!/bin/sh\necho unexpected dependency execution >&2\nexit 97\n')
        denied.chmod(0o700)
        # The fake provider only forwards the explicit private runtime wrapper;
        # it does not select a provider image, installed native host or Bun.
        provider.write_text('''#!/usr/bin/python3
import os, pathlib, sys, tomllib
assert sys.argv[1] == 'interactive'
config = pathlib.Path(sys.argv[sys.argv.index('--config-root')+1])
runtime = tomllib.loads((config/'agent-runner-codex/config.toml').read_text())
wrapper = pathlib.Path(runtime['codex_bin'])
assert wrapper.parent == pathlib.Path(os.environ['HOME'])
os.execv(str(wrapper), [str(wrapper)])
''')
        native.write_text('''#!/usr/bin/python3
import http.client, json, os, pathlib, signal, socket, sys, tomllib, urllib.parse
root = pathlib.Path(os.environ['HOME'])
assert root == pathlib.Path('''+repr(str(output))+''')
assert 'OPENAI_API_KEY' not in os.environ
args = sys.argv[1:]
settings = [tomllib.loads(args[n+1]) for n,a in enumerate(args) if a == '-c']
url = next(s['model_providers']['inventory']['base_url'] for s in settings if 'model_providers' in s)
url = urllib.parse.urlsplit(url)
assert url.hostname == '127.0.0.1'
for index in range('''+str(request_count)+'''):
    connection = http.client.HTTPConnection(url.hostname, url.port)
    body = {'model':'synthetic-wrong-model', 'input':[], 'synthetic_request':index}
    connection.request('POST', '/responses', body=json.dumps(body), headers={'Content-Type':'application/json'})
    response = connection.getresponse()
    assert response.status == 200
    assert b'response.completed' in response.read()
    connection.close()
report = {'schema_version':1, 'token':'local-inventory-token',
          'invocation_uuid':json.loads(os.environ['OULIPOLY_PARENT_INVOCATION'])['id'],
          'provider_session_id':'cccccccc-cccc-4ccc-8ccc-cccccccccccc'}
with socket.socket(socket.AF_UNIX) as client:
    client.connect(os.environ['OULIPOLY_LIVE_SESSION_BIND_SOCKET'])
    client.sendall((json.dumps(report)+'\\n').encode())
    with client.makefile('rb') as stream: assert json.loads(stream.readline())['ok']
print('SYNTHETIC-NATIVE-FAILURE', flush=True)
'''+("(root/'bash-calls.jsonl').write_text('')\nsignal.pause()\n" if validation_failure else 'sys.exit(23)\n'))
        for executable in [provider, native]: executable.chmod(0o700)
        completed = subprocess.run(
            ['/usr/bin/python3', str(REPO/'tests/verify_codex_tui_inventory.py'),
             '--boundary-report', str(self.private/'boundary.json'),
             '--binary', str(provider), '--native-codex', str(native),
             '--bun', str(denied), '--output-dir', str(output)],
            env=dict(os.environ), capture_output=True)
        (self.root/'producer.stdout').write_bytes(completed.stdout)
        (self.root/'producer.stderr').write_bytes(completed.stderr)
        (self.root/'producer.rc').write_text(str(completed.returncode)+'\n')
        self.assertEqual(completed.returncode, 1, completed.stderr)
        if validation_failure:
            self.assertIn(b"assert body['model']==model", completed.stderr)
            self.assertNotIn(b'Native TUI exited', completed.stderr)
        else:
            self.assertIn(b'Native TUI exited with 5888; artifacts:', completed.stderr)
        self.assertIn(b'SYNTHETIC-NATIVE-FAILURE', (output/'pty.raw').read_bytes())
        reports = json.loads((output/'binding-reports.json').read_text())
        self.assertEqual(reports, [{'schema_version':1, 'token':'local-inventory-token',
            'invocation_uuid':inventory.INVOCATION_UUID,
            'provider_session_id':'cccccccc-cccc-4ccc-8ccc-cccccccccccc'}])
        self.assertFalse((output/'result.json').exists())
        if request_count:
            self.assertEqual(json.loads((output/'requests.json').read_text()),
                [{'model':'synthetic-wrong-model', 'input':[], 'synthetic_request':n}
                 for n in range(request_count)])
        else:
            self.assertFalse((output/'requests.json').exists(), 'no capture must not invent a request artifact')

    def test_early_child_exit_preserves_first_request(self):
        self.run_failure(1)

    def test_early_child_exit_preserves_all_captured_requests(self):
        self.run_failure(2)

    def test_validation_failure_preserves_requests_and_original_assertion(self):
        self.run_failure(2, validation_failure=True)

    def test_exit_without_capture_does_not_invent_requests(self):
        self.run_failure(0)


if __name__ == '__main__': unittest.main()

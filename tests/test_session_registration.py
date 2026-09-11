#!/usr/bin/env python3
"""Private deterministic AGE356 fixtures. No native UI, models, or account state.
--native-probe optionally supplies the compiled tests/native/registration_probe.rs
linked to the unmodified pinned upstream crates; fake callbacks are not discovery proof.
"""
import argparse
import json
import os
from pathlib import Path
import socket
import select
import subprocess
import tempfile
import threading
import time
import unittest

REPO = Path(__file__).resolve().parents[1]
BINARY = BUN = PROBE = None
ID = 'cccccccc-cccc-4ccc-8ccc-cccccccccccc'
INV = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
NATIVE = r'''#!/usr/bin/python3
import json,os,sys
with open(os.environ['CALLS']+'.all','a') as f: f.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1:] == ['--version']:
 print('codex-cli 0.153.4');sys.exit(0)
if sys.argv[1:2] in [['app-server'], ['features'], ['doctor'], ['debug']]:
 sys.exit(93) # No extra config/runtime startup is part of interactive preparation.
with open(os.environ['CALLS'],'w') as f: json.dump({'argv':sys.argv[1:],'env':dict(os.environ)},f)
'''

class Fixture:
    def __init__(self):
        self.temp = tempfile.TemporaryDirectory(prefix="age356 'quote $-", dir='/tmp')
        self.root = Path(self.temp.name)
        self.home = self.root / 'home'
        self.cwd = self.root / 'project.with.dots'
        self.config = self.root / 'config'
        for p in [self.home / '.codex3/sessions', self.cwd / '.git', self.cwd / '.codex', self.config / 'agent-runner-codex', self.root / 'data', self.root / 'system']:
            p.mkdir(parents=True, exist_ok=True)
        self.native = self.root / 'native'
        self.native.write_text(NATIVE); self.native.chmod(0o700)
        self.dep = self.root / 'dependency'; self.dep.write_text('#!/bin/sh\nexit 0\n'); self.dep.chmod(0o700)
        self.prompt = self.root / 'prompt'; self.prompt.write_text('fixture system instructions')
        self.paths = dict(codex_bin=self.native, bun_bin=BUN, bash_mcp_path=self.config/'agent-runner-codex/integrations/codex/agent-bash-mcp.ts', system_prompt_file=self.prompt, agent_bash_bin=self.dep, agent_runner_bin=self.dep)
        self.write_config()
        (self.config/'providers.toml').write_text('[codex3]\ntool_restrictions={kind="codex"}\n')
        self.env = dict(PATH='/usr/bin:/bin', HOME=str(self.home), XDG_CONFIG_HOME=str(self.config), XDG_DATA_HOME=str(self.root/'data'), SHELL='/bin/sh', CALLS=str(self.root/'calls.json'), OULIPOLY_PARENT_INVOCATION=json.dumps({'id':INV,'source':'fixture'}), OULIPOLY_LIVE_SESSION_BIND_TOKEN='private-fixture-token', OULIPOLY_LIVE_SESSION_BIND_SOCKET=str(self.root/'s.sock'), FIXTURE_SESSION=ID)
        self.stop = threading.Event(); self.thread = None; self.reports=[]
    def write_config(self):
        (self.config/'agent-runner-codex/config.toml').write_text(''.join(f'{k}={json.dumps(str(v))}\n' for k,v in self.paths.items()))
    def launch(self):
        p = subprocess.run([str(BINARY),'interactive','--settings-id','codex3','--config-root',str(self.config)],env=self.env,cwd=self.cwd,capture_output=True,text=True,timeout=25)
        if p.returncode == 0:
            call=json.loads((self.root/'calls.json').read_text());self.live=call['env'];self.live['FIXTURE_NATIVE_ARGS']=json.dumps(call['argv']);self.selected=Path(self.live['CODEX_HOME'])
        return p
    def rollout(self, cwd=None, id=ID):
        path=self.home/'.codex3/sessions/rollout-fixture.jsonl'
        path.write_text(json.dumps({'type':'session_meta','payload':{'id':id,'cwd':str(cwd or self.cwd),'timestamp':'2026-09-10T00:00:00Z'}})+'\n')
    def capture(self, report):
        req=dict(contract='oulipoly.provider/v1',request_id='fixture-capture',provider_instance_id='codex3',host=dict(app='fixture',config_root=str(self.config),working_directory=str(self.cwd),env={'HOME':str(self.home)}),params=dict(settings_id='codex3',invocation_uuid=INV,live_report={k:report[k] for k in ['invocation_uuid','provider_session_id']}))
        p=subprocess.run([str(BINARY),'session.capture'],input=json.dumps(req),capture_output=True,text=True,env=self.env,cwd=self.cwd,timeout=3)
        return p.returncode==0 and json.loads(p.stdout).get('result',{}).get('provider_session_id')==report['provider_session_id']
    def receiver(self, corrupt=None):
        listener=socket.socket(socket.AF_UNIX);listener.bind(self.env['OULIPOLY_LIVE_SESSION_BIND_SOCKET']);listener.listen();listener.settimeout(.1)
        def serve():
            acknowledged=None
            try:
                while not self.stop.is_set():
                    try: client,_=listener.accept()
                    except socket.timeout: continue
                    with client:
                        client.settimeout(2);line=b''
                        while b'\n' not in line and len(line)<16384:
                            b=client.recv(4096)
                            if not b:break
                            line+=b
                        try:
                            r=json.loads(line);self.reports.append(r)
                            valid=r['token']==self.env['OULIPOLY_LIVE_SESSION_BIND_TOKEN'] and r['invocation_uuid']==INV and (r['provider_session_id']==acknowledged if acknowledged else self.capture(r))
                            if valid: acknowledged=r['provider_session_id']
                            ack=dict(ok=valid,session_id=acknowledged,provider_session_id=acknowledged,agent_runner_invocation_id=INV)
                            if corrupt: ack.update(corrupt)
                            client.sendall((json.dumps(ack)+'\n').encode())
                        except (OSError,ValueError,KeyError): pass
            finally:listener.close()
        self.thread=threading.Thread(target=serve);self.thread.start()
    def helper(self, id=ID, cwd=None, timeout=450):
        helper=self.selected/'integrations/codex/session-registration.ts'
        code=f'import {{registerSession}} from {json.dumps(str(helper))}; await registerSession({json.dumps(id)},{json.dumps(str(cwd or self.cwd))},{timeout})'
        return subprocess.run([str(BUN),'--no-install','-e',code],env=self.live,cwd=self.cwd,capture_output=True,text=True,timeout=4)
    def probe(self, mode='on'):
        return subprocess.run([str(PROBE),str(self.root),str(self.selected),str(self.cwd),mode],env=self.live,cwd=self.cwd,capture_output=True,text=True,timeout=40)
    def close(self):
        self.stop.set()
        if self.thread:self.thread.join(4);assert not self.thread.is_alive()
        self.temp.cleanup()

class RegistrationTests(unittest.TestCase):
    def setUp(self):self.f=Fixture()
    def tearDown(self):self.f.close()
    def launch(self):
        p=self.f.launch();self.assertEqual(p.returncode,0,p.stderr);return p
    def test_missing_default_payload_is_repaired_without_mutating_install(self):
        self.assertFalse(self.f.paths['bash_mcp_path'].exists());self.launch()
        self.assertFalse(self.f.paths['bash_mcp_path'].exists())
        self.assertEqual((self.f.selected/'integrations/codex/session-registration.ts').read_bytes(),(REPO/'integrations/codex/session-registration.ts').read_bytes())
        old=self.f.selected; self.launch(); self.assertNotEqual(old,self.f.selected)
        self.assertEqual((old/'integrations/codex/session-registration.ts').read_bytes(),(REPO/'integrations/codex/session-registration.ts').read_bytes())
    def test_symlinked_account_uses_physical_selected_home(self):
        account=self.f.home/'.codex3';physical=self.f.root/'physical-account';account.rename(physical);account.symlink_to(physical,target_is_directory=True)
        self.launch();self.assertEqual(self.f.selected,self.f.selected.resolve());self.assertTrue(self.f.selected.is_relative_to(physical))
        self.assertIn(str(self.f.selected/'config.toml'),(self.f.selected/'config.toml').read_text())
    def test_stale_default_payload_preserved_but_not_executed(self):
        p=self.f.paths['bash_mcp_path'];p.parent.mkdir(parents=True);p.write_text('STALE')
        self.launch();self.assertEqual(p.read_text(),'STALE')
        self.assertEqual((self.f.selected/'integrations/codex/agent-bash-mcp.ts').read_bytes(),(REPO/'integrations/codex/agent-bash-mcp.ts').read_bytes())
    def test_custom_incoherent_integration_fails_before_native(self):
        p=self.f.root/'custom.ts';p.write_text('do not replace');self.f.paths['bash_mcp_path']=p;self.f.write_config()
        result=self.f.launch();self.assertNotEqual(result.returncode,0);self.assertIn('install-provider.py',result.stderr)
        self.assertFalse((self.f.root/'calls.json.all').exists());self.assertEqual(p.read_text(),'do not replace')
    def test_nonexecutable_dependency_fails_before_native(self):
        self.f.dep.chmod(0o600);self.assertNotEqual(self.f.launch().returncode,0);self.assertFalse((self.f.root/'calls.json').exists())
    def test_one_normal_startup_and_version_only_no_native_config_probe(self):
        self.launch()
        calls=[json.loads(line) for line in (self.f.root/'calls.json.all').read_text().splitlines()]
        self.assertEqual(calls, [['--version'], json.loads(self.f.live['FIXTURE_NATIVE_ARGS'])])
        self.assertNotIn('CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED',self.f.live)
    def test_missing_runner_authority_fails_before_native(self):
        del self.f.env['OULIPOLY_LIVE_SESSION_BIND_TOKEN']
        self.assertNotEqual(self.f.launch().returncode,0)
        self.assertFalse((self.f.root/'calls.json').exists())
    def test_concurrent_launch_generations_remain_coherent(self):
        # Concurrent preparers share account stores, never a published integration tree.
        commands=[]
        for i in range(3):
            env={**self.f.env,'CALLS':str(self.f.root/f'calls-{i}.json')}
            commands.append(subprocess.Popen([str(BINARY),'interactive','--settings-id','codex3','--config-root',str(self.f.config)],env=env,cwd=self.f.cwd,stdout=subprocess.PIPE,stderr=subprocess.PIPE))
        homes=[]
        try:
            for i,p in enumerate(commands):
                out,err=p.communicate(timeout=25);self.assertEqual(p.returncode,0,err)
                homes.append(Path(json.loads((self.f.root/f'calls-{i}.json').read_text())['env']['CODEX_HOME']))
        finally:
            for p in commands:
                if p.poll() is None:p.kill();p.wait()
        self.assertEqual(len(set(homes)),3)
        for home in homes:
            self.assertEqual((home/'integrations/codex/session-registration.ts').read_bytes(),(REPO/'integrations/codex/session-registration.ts').read_bytes())
            self.assertTrue((home/'config.toml').exists())
    def test_exact_capture_no_tool_and_duplicate_ack(self):
        self.launch();self.f.rollout();self.f.receiver()
        self.assertNotIn('CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED',self.f.live)
        for _ in range(2):
            p=self.f.helper();self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(p.stdout,'')
        self.assertEqual(len(self.f.reports),2)
    def test_staged_bash_bridge_loads_quoted_generation_paths(self):
        self.launch()
        script=self.f.selected/'integrations/codex/agent-bash-mcp.ts'
        p=subprocess.Popen([str(BUN),'--no-install',str(script)],env=self.f.live,cwd=self.f.cwd,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        try:
            for request in [{'jsonrpc':'2.0','id':1,'method':'initialize','params':{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'fixture','version':'1'}}},{'jsonrpc':'2.0','id':2,'method':'tools/list'}]:
                p.stdin.write(json.dumps(request)+'\n');p.stdin.flush()
                self.assertTrue(select.select([p.stdout],[],[],4)[0],'staged bridge response deadline')
                line=p.stdout.readline();self.assertTrue(line,'staged bridge exited');reply=json.loads(line)
                self.assertEqual(reply['id'],request['id'])
                if request['id']==2:self.assertEqual([t['name'] for t in reply['result']['tools']],['bash'])
        finally:
            p.stdin.close()
            try:p.wait(timeout=4)
            except subprocess.TimeoutExpired:p.kill();p.wait()
            p.stdout.close();p.stderr.close()
    def test_delayed_rollout_retries_unchanged_capture(self):
        self.launch();self.f.receiver()
        timer=threading.Timer(.18,self.f.rollout);timer.start()
        try:p=self.f.helper(timeout=1500)
        finally:timer.join()
        self.assertEqual(p.returncode,0,p.stderr);self.assertGreaterEqual(len(self.f.reports),2)
        self.assertTrue(all(r==self.f.reports[0] for r in self.f.reports))
    def test_wrong_cwd_and_metadata_mismatch_reject_capture(self):
        self.launch();self.f.rollout(cwd=self.f.root);self.f.receiver()
        self.assertNotEqual(self.f.helper().returncode,0)
        self.assertNotEqual(self.f.helper(cwd=self.f.root).returncode,0)
        self.f.rollout(id='dddddddd-dddd-4ddd-8ddd-dddddddddddd')
        self.assertNotEqual(self.f.helper().returncode,0)
    def test_redirected_metadata_is_not_adopted_or_latest_guessed(self):
        self.launch()
        redirected=self.f.root/'redirected-store';redirected.mkdir()
        (redirected/'rollout.jsonl').write_text(json.dumps({'type':'session_meta','payload':{'id':ID,'cwd':str(self.f.cwd),'timestamp':'2026-09-10T00:00:00Z'}})+'\n')
        self.f.rollout(id='dddddddd-dddd-4ddd-8ddd-dddddddddddd')
        self.f.receiver()
        p=self.f.helper();self.assertNotEqual(p.returncode,0)
        self.assertIn('no exact authorized acknowledgement',p.stderr)
        self.assertTrue(self.f.reports)
    def test_refused_socket_never_implies_prior_binding(self):
        self.launch();p=self.f.helper();self.assertNotEqual(p.returncode,0)
        self.assertIn('no exact authorized acknowledgement',p.stderr)
        self.assertIn('does not establish effective native policy',p.stderr)
    def test_wrong_ack_session_id_is_not_success(self):
        self.launch();self.f.rollout();self.f.receiver({'session_id':'different'})
        self.assertNotEqual(self.f.helper().returncode,0)
    def test_resumed_id_mismatch_is_rejected_without_callback(self):
        self.launch();self.f.rollout();self.f.receiver()
        self.f.live['AGENT_RUNNER_CODEX_SESSION_ID']='dddddddd-dddd-4ddd-8ddd-dddddddddddd'
        self.assertNotEqual(self.f.helper().returncode,0);self.assertEqual(self.f.reports,[])
    def test_wrong_ack_and_conflicting_id_never_succeed(self):
        self.launch();self.f.rollout();self.f.receiver({'agent_runner_invocation_id':'different'})
        self.assertNotEqual(self.f.helper().returncode,0)
        self.assertNotEqual(self.f.helper(id='dddddddd-dddd-4ddd-8ddd-dddddddddddd').returncode,0)

class NativeDiscoveryTests(unittest.TestCase):
    tearDown = RegistrationTests.tearDown
    launch = RegistrationTests.launch
    # Only these explicit tests require native crates; do not rerun inherited helper cases.
    def setUp(self):
        if not PROBE:self.skipTest('pinned native probe not supplied; helper fixtures are not native discovery proof')
        self.f=Fixture()
    def test_actual_native_loader_hash_and_no_tool_engine(self):
        self.launch();self.f.rollout();self.f.receiver()
        (self.f.cwd/'.codex/config.toml').write_text('[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype="command"\ncommand="exit 91"\n')
        p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
        self.assertEqual(v['effective_features']['hooks'],True,v['effective_features'])
        expected={}
        args=json.loads(self.f.live['FIXTURE_NATIVE_ARGS'])
        for i,arg in enumerate(args[:-1]):
            if arg=='-c' and args[i+1].startswith('features.'):
                key,value=args[i+1][len('features.'):].split('=',1);expected[key]=value=='true'
        for key,wanted in expected.items():self.assertEqual(v['effective_features'].get(key),wanted,(key,v['effective_features']))
        self.assertEqual(v['effective_owned_settings'],dict(model_instructions_file=str(self.f.prompt),model_catalog_json=str(self.f.selected/'integrations/codex/models.json'),sqlite_home=str(self.f.home/'.codex3'),cli_auth_credentials_store='file'))
        self.assertEqual(len(v['list']),2);self.assertTrue(all(h['trust']=='Trusted' for h in v['list']),v)
        self.assertEqual((v['start_runs'],v['submit_runs'],v['start_stop'],v['submit_stop'],v['start_contexts'],v['submit_contexts']),(1,1,False,False,0,0));self.assertEqual(len(self.f.reports),2)
    def test_actual_native_missing_trust_and_old_random_home_key(self):
        self.launch()
        import tomllib
        config=self.f.selected/'config.toml';original=config.read_text()
        for text in [original.replace('trusted_hash = "sha256:', 'trusted_hash = "wrong:'),original.replace(str(self.f.selected/'config.toml'),'/old/random/home/config.toml')]:
            config.write_text(text);p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
            self.assertEqual((v['start_runs'],v['submit_runs']),(0,0),v)
    def test_actual_native_changed_declaration_is_modified_not_trusted(self):
        self.launch();config=self.f.selected/'config.toml';config.write_text(config.read_text().replace('timeout = 15','timeout = 14'))
        p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
        self.assertTrue(all(h['trust']=='Modified' for h in v['list']),v);self.assertEqual((v['start_runs'],v['submit_runs']),(0,0))
    def test_actual_native_nonzero_and_timeout_are_advisory_limits(self):
        self.launch()
        import hashlib
        for command,timeout in [('exit 1',1),('sleep 3',1)]:
            text=''
            for event,label in [('SessionStart','session_start'),('UserPromptSubmit','user_prompt_submit')]:
                handler={'type':'command','command':command,'timeout':timeout,'async':False}
                identity={'event_name':label,'hooks':[handler]}
                digest='sha256:'+hashlib.sha256(json.dumps(identity,sort_keys=True,separators=(',',':')).encode()).hexdigest()
                key=str(self.f.selected/'config.toml')+f':{label}:0:0'
                text+=f'[[hooks.{event}]]\n[[hooks.{event}.hooks]]\ntype="command"\ncommand={json.dumps(command)}\ntimeout={timeout}\nasync=false\n[hooks.state.{json.dumps(key)}]\nenabled=true\ntrusted_hash={json.dumps(digest)}\n'
            (self.f.selected/'config.toml').write_text(text)
            p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
            self.assertEqual((v['start_runs'],v['submit_runs']),(1,1));self.assertTrue(all(h['trust']=='Trusted' for h in v['list']),v)
            self.assertFalse(v['start_stop']);self.assertFalse(v['submit_stop'])
    def test_actual_native_feature_off_managed_only_and_system_hook(self):
        self.launch()
        p=self.f.probe('off');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(json.loads(p.stdout)['start_runs'],0)
        (self.f.root/'system/requirements.toml').write_text('allow_managed_hooks_only=true\n')
        (self.f.root/'system/config.toml').write_text('[[hooks.SessionStart]]\n[[hooks.SessionStart.hooks]]\ntype="command"\ncommand="true"\n')
        p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
        self.assertEqual(v['start_runs'],1);self.assertEqual(v['submit_runs'],0);self.assertEqual(v['list'][0]['trust'],'Managed')
    def test_actual_native_structured_stop_repeats_on_next_submit(self):
        self.launch();self.f.live['AGENT_RUNNER_CODEX_REGISTRATION_CWD']=str(self.f.root/'wrong')
        p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
        self.assertTrue(v['start_stop']);self.assertTrue(v['submit_stop']);self.assertEqual(v['start_contexts'],0);self.assertEqual(v['submit_contexts'],0)
    def test_actual_native_failed_helper_wrapper_stops(self):
        self.launch();helper=self.f.selected/'integrations/codex/session-registration.ts';helper.chmod(0o600);helper.write_text('process.exit(1)\n')
        p=self.f.probe();self.assertEqual(p.returncode,0,p.stderr);v=json.loads(p.stdout)
        self.assertTrue(v['start_stop']);self.assertTrue(v['submit_stop'])
        # Native trust is still trusted after changing only script bytes.
        self.assertTrue(all(h['trust']=='Trusted' for h in v['list']),v)

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('--binary',required=True,type=Path);parser.add_argument('--bun',required=True,type=Path);parser.add_argument('--native-probe',type=Path)
    args,rest=parser.parse_known_args();BINARY=args.binary.resolve();BUN=args.bun.resolve();PROBE=args.native_probe.resolve() if args.native_probe else None
    unittest.main(argv=[__file__,*rest])

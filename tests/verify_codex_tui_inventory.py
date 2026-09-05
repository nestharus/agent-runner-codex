#!/usr/bin/env python3
"""Exercise managed native TUI, MCP identity and config isolation using loopback only."""
import argparse
import errno
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import runpy
import select
import shutil
import signal
import socket
import struct
import tempfile
import termios
import threading
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/debug/agent-runner-codex'))
    parser.add_argument('--label', default='gpt-luna-low', choices=['gpt-luna-low', 'gpt-luna-max', 'gpt-xhigh'])
    parser.add_argument('--output-dir', type=Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    repo = Path(__file__).resolve().parents[1]
    root = (args.output_dir or Path(tempfile.mkdtemp(prefix='codex-tui-inventory-'))).absolute()
    root.mkdir(parents=True, exist_ok=True)
    captured = []
    reports = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            captured.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            events = []
            if len(captured) == 1:
                events.append({'type':'response.output_item.done', 'item':{'type':'function_call', 'call_id':'inventory-bash-call', 'namespace':'mcp__agent_bash', 'name':'bash', 'arguments':json.dumps({'command':'printf inventory-tool-call'})}})
            else:
                events.append({'type':'response.output_item.done','item':{'id':'inventory-final','type':'message','role':'assistant','phase':'final_answer','content':[{'type':'output_text','text':'TUI-INVENTORY-COMPLETE'}]}})
            events.append({'type':'response.completed','response':{'id':f'resp_local_{len(captured)}','object':'response','status':'completed','output':[], 'usage':{'input_tokens':1,'output_tokens':1,'total_tokens':2}}})
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.wfile.write(''.join('event: '+e['type']+'\ndata: '+json.dumps(e)+'\n\n' for e in events).encode())
        def log_message(self, *_): pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    account = root/'.codex'
    account.mkdir()
    workspace = root/'project'
    (workspace/'.codex').mkdir(parents=True)
    (workspace/'.git').mkdir()
    canary = '[mcp_servers.project_extra]\ncommand='+json.dumps(shutil.which('bun'))+'\nargs='+json.dumps(['--no-install',str(repo/'integrations/codex/agent-bash-mcp.ts')])+'\nrequired=true\n[features]\nshell_tool=true\nmulti_agent=true\n'
    (workspace/'.codex/config.toml').write_text(canary)
    # Both user and project canaries would expose extra execution tools if the
    # native TUI picked up the original account/project configuration.
    (account/'config.toml').write_text(canary.replace('project_extra','user_extra')+'\n[projects.'+json.dumps(str(workspace))+']\ntrust_level="trusted"\n')
    config = root/'config'
    (config/'agent-runner-codex').mkdir(parents=True)
    (config/'providers.toml').write_text('[codex]\nsystem_prompt_override="TUI-DEVELOPER-SENTINEL"\ntool_restrictions={kind="codex"}\n')
    spooler = root/'agent-bash'
    spooler.write_text(runpy.run_path(str(repo/'integrations/codex/test_mcp.py'))['FAKE'])
    spooler.chmod(0o755)
    wrapper = root/'native-codex'
    override = 'model_providers.inventory={name="inventory",base_url="http://127.0.0.1:'+str(server.server_port)+'",wire_api="responses",requires_openai_auth=false}'
    wrapper.write_text('#!/usr/bin/env python3\nimport os,sys,json\nargs=sys.argv[1:]\nif args != ["--version"]:\n with open('+repr(str(root/'native-call.json'))+',"w") as f: json.dump({"args":args,"home":os.environ["CODEX_HOME"]},f)\n separator=args.index("--") if "--" in args else len(args)\n args[separator:separator]=["-c",\'model_provider="inventory"\',"-c",'+repr(override)+']\nos.execv('+repr(shutil.which('codex'))+',["codex",*args])\n')
    wrapper.chmod(0o755)
    runtime = {'codex_bin':str(wrapper),'bun_bin':shutil.which('bun'),'bash_mcp_path':str(repo/'integrations/codex/agent-bash-mcp.ts'),'system_prompt_file':str(Path.home()/'ai/AGENTS.md'),'agent_bash_bin':str(spooler),'agent_runner_bin':str(Path.home()/'.local/bin/agents')}
    (config/'agent-runner-codex/config.toml').write_text(''.join(k+'='+json.dumps(v)+'\n' for k,v in runtime.items()))
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(root/'binding.sock'))
    listener.listen(2)
    listener.settimeout(40)
    def acknowledge():
        try:
            connection, _ = listener.accept()
            with connection:
                report = json.loads(connection.makefile('rb').readline())
                reports.append(report)
                connection.sendall((json.dumps({'ok':True,'session_id':report['provider_session_id']})+'\n').encode())
        except (OSError, ValueError) as error:
            reports.append({'error':str(error)})
    threading.Thread(target=acknowledge, daemon=True).start()
    environment = dict(os.environ)
    for key in ['OULIPOLY_PARENT_INVOCATION','OULIPOLY_LIVE_SESSION_BIND_SOCKET','OULIPOLY_LIVE_SESSION_BIND_TOKEN','AGENT_RUNNER_CODEX_SESSION_ID','AGENT_RUNNER_CODEX_SESSION_FILE','AGENT_RUNNER_CODEX_SESSION_BINDING','AGENT_RUNNER_CODEX_INTERACTIVE']:
        environment.pop(key,None)
    environment.update({'HOME':str(root),'TERM':'xterm-256color','FAKE_LOG':str(root/'bash-calls.jsonl'),'AGENT_BASH_TOOL_POLL_MS':'25','OULIPOLY_PARENT_INVOCATION':json.dumps({'id':'native-tui-inventory'}),'OULIPOLY_LIVE_SESSION_BIND_SOCKET':str(root/'binding.sock'),'OULIPOLY_LIVE_SESSION_BIND_TOKEN':'local-inventory-token','CODEX_THREAD_ID':'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'})
    pid, master = pty.fork()
    if pid == 0:
        fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,0,0))
        os.chdir(workspace)
        os.execve(str(args.binary.resolve()),[str(args.binary.resolve()),'interactive','--settings-id','codex','--config-root',str(config),'--model',args.label,'--prompt','Call Bash once, then finish.'],environment)
    reaped=False
    terminal=b''
    try:
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            ready,_,_=select.select([master],[],[],0.1)
            if ready:
                try: chunk=os.read(master,65536)
                except OSError as error:
                    if error.errno!=errno.EIO:raise
                    chunk=b''
                terminal+=chunk
                for query,reply in [(b'\x1b[6n',b'\x1b[1;1R'),(b'\x1b[c',b'\x1b[?1;2c'),(b'\x1b[>c',b'\x1b[>0;0;0c'),(b'\x1b[?u',b'\x1b[?0u')]:
                    if query in chunk: os.write(master,reply)
            if len(captured)>=2 and reports and (root/'bash-calls.jsonl').exists():break
            ended,status=os.waitpid(pid,os.WNOHANG)
            if ended:
                reaped=True
                raise AssertionError(f'Native TUI exited with {status}; artifacts: {root}')
        (root/'pty.raw').write_bytes(terminal)
        (root/'requests.json').write_text(json.dumps(captured,indent=2)+'\n')
        assert len(captured)==2, f'Expected native Bash call + response, received {len(captured)} requests; artifacts: {root}'
        body=captured[0]
        model='gpt-6-astra' if args.label=='gpt-xhigh' else 'gpt-5.6-luna'
        assert body['model']==model
        assert body['reasoning']['effort']==args.label.rsplit('-',1)[1]
        tools=list(body.get('tools',[]))
        for item in body.get('input',[]):
            if item.get('type')=='additional_tools':tools.extend(item.get('tools',[]))
        names=[]
        for tool in tools:
            if tool.get('type')=='namespace':names.extend(tool['name']+'.'+child['name'] for child in tool.get('tools',[]))
            else:names.append(tool.get('name',tool.get('type')))
        allowed={'mcp__agent_bash.bash','functions.request_user_input','functions.list_mcp_resources','functions.list_mcp_resource_templates','functions.read_mcp_resource'}
        assert 'mcp__agent_bash.bash' in names and set(names)<=allowed,names
        instructions=Path(runtime['system_prompt_file']).read_text().rstrip()
        texts=[''.join(c.get('text','') for c in i.get('content',[]) if isinstance(c,dict)) for i in body.get('input',[]) if i.get('role')=='developer' and isinstance(i.get('content'),list)]
        assert instructions in texts or body.get('instructions','').rstrip()==instructions,'System prompt differs'
        assert any('TUI-DEVELOPER-SENTINEL' in t for t in texts),'Account instructions missing'
        calls=[json.loads(line) for line in (root/'bash-calls.jsonl').read_text().splitlines()]
        session=reports[0]['provider_session_id']
        assert reports==[{'schema_version':1,'token':'local-inventory-token','invocation_uuid':'native-tui-inventory','provider_session_id':session}],reports
        assert session!=environment['CODEX_THREAD_ID']
        assert calls[0]['owner']==session,calls
        assert any(c['args'][0]=='consume' for c in calls),calls
        assert any(i.get('type')=='function_call_output' and 'fixture-output' in json.dumps(i) for i in captured[1]['input']),captured[1]['input']
        # Identity originates in native metadata; verify it against the exact
        # rollout ID after binding rather than discovering a latest-session ID.
        matches=[]
        for path in (account/'sessions').rglob('rollout-*.jsonl'):
            first=json.loads(path.open().readline())
            if first.get('payload',{}).get('id')==session:matches.append(path)
        assert len(matches)==1,matches
        assert json.loads(matches[0].open().readline())['payload']['cwd']==str(workspace)
        result={'passed':True,'mode':'native_tui','model':model,'label':args.label,'tools':names,'system_prompt_exact':True,'account_instructions':True,'session_id':session,'metadata_binding':True,'original_user_and_project_mcp_excluded':True,'artifacts':str(root)}
        (root/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps(result,indent=2))
    finally:
        (root/'pty.raw').write_bytes(terminal)
        if not reaped:
            os.kill(pid,signal.SIGTERM)
            stop=time.monotonic()+3
            while time.monotonic()<stop:
                if os.waitpid(pid,os.WNOHANG)[0]:reaped=True;break
                time.sleep(0.05)
            if not reaped:
                os.killpg(pid,signal.SIGKILL)
                os.waitpid(pid,0)
        os.close(master)
        listener.close()
        server.shutdown()


if __name__=='__main__':main()

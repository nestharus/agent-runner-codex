#!/usr/bin/env python3
"""Native TUI inventory producer; requires an externally isolated, explicit host.

No PATH-discovered Codex/Runner, inherited credentials or production instructions.
Loopback alone is not a sandbox: execute only inside a private user/net/mount/PID
namespace with production homes/state masked and read-only candidate dependencies.
Synthetic producer tests do not establish external native-host behavior.
"""
import argparse
import errno
import fcntl
import hashlib
import http.server
import json
import os
from pathlib import Path
import pty
import runpy
import select
import signal
import socket
import struct
import tempfile
import termios
import threading
import time
import uuid


INVOCATION_UUID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
FIXTURE_OUTPUT = b"fixture-output\n"


def fixture_environment(root):
    """Standalone fixture setup; importing FAKE does not run unittest.setUp."""
    environment = {"PATH": "/usr/bin:/bin", "HOME": str(root), "TERM": "xterm-256color"}
    for name, directory in (("XDG_CONFIG_HOME", "xdg-config"),
                            ("XDG_DATA_HOME", "xdg-data"),
                            ("XDG_STATE_HOME", "xdg-state"),
                            ("XDG_CACHE_HOME", "xdg-cache"),
                            ("XDG_RUNTIME_DIR", "xdg-run"),
                            ("CODEX_HOME", ".codex"), ("TMPDIR", "tmp")):
        path = root / directory
        path.mkdir(mode=0o700)
        environment[name] = str(path)
    repo = Path(__file__).resolve().parents[1]
    spooler = root / "agent-bash"
    spooler.write_text(runpy.run_path(str(repo / "integrations/codex/test_mcp.py"))["FAKE"])
    spooler.chmod(0o700)
    runner = root / "agents"
    runner.write_text("#!/bin/sh\nprintf 'unexpected runner execution\\n' >&2\nexit 97\n")
    runner.chmod(0o700)
    prompt = root / "system-prompt.txt"
    prompt.write_text("Synthetic inventory system instructions.\n")
    environment.update(AGENT_BASH_BIN=str(spooler), AGENT_BASH_AGENT_RUNNER_BIN=str(runner),
                       FAKE_LOG=str(root / "bash-calls.jsonl"), AGENT_BASH_TOOL_POLL_MS="25")
    return environment, spooler, runner, prompt


def assert_retained_result(calls, inputs):
    """Check this fake snapshot/local receipt, never remote settlement or durability."""
    operations = [call["args"][0] for call in calls]
    assert "consume" not in operations, calls
    assert operations.count("run") == 1, calls
    assert operations.count("snapshot") == operations.count("accept-output") == 1, calls
    acquired = operations.index("snapshot")
    accepted = operations.index("accept-output")
    assert acquired < accepted, calls
    assert calls[acquired]["args"] == ["snapshot", "ab_test"], calls
    receipt_args = calls[accepted]["args"]
    assert receipt_args[:3] == ["accept-output", "ab_test", "--snapshot"] and len(receipt_args) == 4, calls
    snapshot = json.loads(receipt_args[3])
    expected = {"version": 1, "handle": "ab_test", "created_at_unix_ms": 1,
                "bytes": len(FIXTURE_OUTPUT), "sha256": hashlib.sha256(FIXTURE_OUTPUT).hexdigest(), "encoding": "hex"}
    assert snapshot == expected, snapshot
    progression = [n for n, call in enumerate(calls)
                   if call["args"][0] == "status" and "--observe-only" not in call["args"]]
    assert progression and all(n > accepted for n in progression), calls
    outputs = [item["output"] for item in inputs if item.get("type") == "function_call_output"
               and item.get("call_id") == "inventory-bash-call"]
    assert len(outputs) == 1, inputs
    output = outputs[0]
    # Native hosts may encode the MCP content as JSON, or flatten its text.
    if isinstance(output, str):
        try: output = json.loads(output)
        except ValueError: pass
    if isinstance(output, dict):
        assert not output.get("isError"), output
        output = output["content"]
    if isinstance(output, list):
        output = "".join(item["text"] for item in output if item.get("type") == "text")
    assert isinstance(output, str), output
    header, body = output.split("\n--- output ---\n", 1)
    assert body == FIXTURE_OUTPUT.decode(), body
    assert "local receipt: durable bounded snapshot; remote ACK: unconfirmed; physical drain: unconfirmed" in header, header
    assert "progression: requested (not remote settlement evidence)" in header, header
    assert "acquired bounded bytes only, not an atomic historical log; later append data unknown" in header, header
    snapshots = [json.loads(line.removeprefix("snapshot: ")) for line in header.splitlines() if line.startswith("snapshot: ")]
    assert snapshots == [expected], snapshots


class BindingReceiver:
    """Synthetic identity ACK using the existing registration-helper contract.

    Hooks and MCP can repeat the same report. This is not a Runner capture or a
    tool-body receipt; the native producer separately checks the exact rollout.
    """
    def __init__(self, path):
        self.reports = []
        self.stop = threading.Event()
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(path))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.thread = threading.Thread(target=self.serve)
        self.thread.start()

    def serve(self):
        session = None
        while not self.stop.is_set():
            try: connection, _ = self.listener.accept()
            except socket.timeout: continue
            with connection:
                connection.settimeout(2)
                try:
                    with connection.makefile("rb") as stream:
                        line = stream.readline(16385)
                    assert len(line) <= 16384 and line.endswith(b"\n")
                    report = json.loads(line)
                    self.reports.append(report)
                    candidate = report["provider_session_id"]
                    # Validate this fixture's exact report, including a native UUID.
                    assert str(uuid.UUID(candidate)) == candidate
                    assert report == {"schema_version": 1, "token": "local-inventory-token",
                                      "invocation_uuid": INVOCATION_UUID, "provider_session_id": candidate}
                    assert session is None or candidate == session
                    session = candidate
                    reply = {"ok": True, "session_id": session, "provider_session_id": session,
                             "agent_runner_invocation_id": INVOCATION_UUID}
                except (OSError, ValueError, KeyError, AssertionError, TypeError):
                    reply = {"ok": False}
                try: connection.sendall((json.dumps(reply)+"\n").encode())
                except OSError: pass

    def close(self):
        self.stop.set()
        self.thread.join()
        self.listener.close()


def explicit_executable(value):
    path = Path(value)
    if not path.is_absolute() or not path.is_file() or not os.access(path, os.X_OK):
        raise argparse.ArgumentTypeError("requires an existing absolute executable in the private environment")
    return path.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=explicit_executable, required=True)
    parser.add_argument('--native-codex', type=explicit_executable, required=True, help='Explicit isolated native host; never discovered through PATH')
    parser.add_argument('--bun', type=explicit_executable, required=True)
    parser.add_argument('--label', default='gpt-luna-low', choices=[prefix+e for prefix in ['gpt-', 'gpt-astra-', 'gpt-luna-', 'gpt-terra-', 'gpt-sol-'] for e in ['low','medium','high','xhigh','max']])
    parser.add_argument('--no-model', action='store_true', help='Omit --model to check the managed gpt-xhigh default (Sol/xhigh)')
    parser.add_argument('--output-dir', type=Path)
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    if args.no_model:
        args.label = 'gpt-xhigh'
    effort = args.label.rsplit('-', 1)[1]
    repo = Path(__file__).resolve().parents[1]
    root = (args.output_dir or Path(tempfile.mkdtemp(prefix='codex-tui-inventory-'))).absolute()
    if args.output_dir:
        root.mkdir(mode=0o700, parents=True, exist_ok=False)
    environment, spooler, runner, prompt = fixture_environment(root)
    captured = []
    capture_lock = threading.Lock()

    def persist_requests(requests):
        # Publish a complete snapshot without truncating prior failure evidence.
        pending = root/'requests.json.tmp'
        pending.write_text(json.dumps(requests, indent=2)+'\n')
        pending.replace(root/'requests.json')

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            with capture_lock:
                # Retain before replying or exposing the capture count to the PTY
                # loop: early exit, response failure and cleanup errors must not
                # bypass evidence that this isolated fixture already received.
                persist_requests([*captured, request])
                captured.append(request)
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
    workspace = root/'project'
    (workspace/'.codex').mkdir(parents=True)
    (workspace/'.git').mkdir()
    canary = '[mcp_servers.project_extra]\ncommand='+json.dumps(str(args.bun))+'\nargs='+json.dumps(['--no-install',str(repo/'integrations/codex/agent-bash-mcp.ts')])+'\nrequired=true\n[features]\nshell_tool=true\nmulti_agent=true\n'
    (workspace/'.codex/config.toml').write_text(canary)
    # Both user and project canaries would expose extra execution tools if the
    # native TUI picked up the original account/project configuration.
    (account/'config.toml').write_text(canary.replace('project_extra','user_extra')+'\n[projects.'+json.dumps(str(workspace))+']\ntrust_level="trusted"\n')
    config = root/'config'
    (config/'agent-runner-codex').mkdir(parents=True)
    (config/'providers.toml').write_text('[codex]\nsystem_prompt_override="TUI-DEVELOPER-SENTINEL"\ntool_restrictions={kind="codex"}\n')
    wrapper = root/'native-codex'
    override = 'model_providers.inventory={name="inventory",base_url="http://127.0.0.1:'+str(server.server_port)+'",wire_api="responses",requires_openai_auth=false}'
    wrapper.write_text('#!/usr/bin/python3\nimport os,sys,json\nargs=sys.argv[1:]\nif args != ["--version"]:\n with open('+repr(str(root/'native-call.json'))+',"w") as f: json.dump({"args":args,"home":os.environ["CODEX_HOME"]},f)\n separator=args.index("--") if "--" in args else len(args)\n args[separator:separator]=["-c",\'model_provider="inventory"\',"-c",'+repr(override)+']\nos.execv('+repr(str(args.native_codex))+',["codex",*args])\n')
    wrapper.chmod(0o755)
    runtime = {'codex_bin':str(wrapper),'bun_bin':str(args.bun),'bash_mcp_path':str(repo/'integrations/codex/agent-bash-mcp.ts'),'system_prompt_file':str(prompt),'agent_bash_bin':str(spooler),'agent_runner_bin':str(runner)}
    (config/'agent-runner-codex/config.toml').write_text(''.join(k+'='+json.dumps(v)+'\n' for k,v in runtime.items()))
    receiver = BindingReceiver(root/'binding.sock')
    reports = receiver.reports
    environment.update({'HOME':str(root),'TERM':'xterm-256color','FAKE_LOG':str(root/'bash-calls.jsonl'),'AGENT_BASH_TOOL_POLL_MS':'25','OULIPOLY_PARENT_INVOCATION':json.dumps({'id':INVOCATION_UUID}),'OULIPOLY_LIVE_SESSION_BIND_SOCKET':str(root/'binding.sock'),'OULIPOLY_LIVE_SESSION_BIND_TOKEN':'local-inventory-token','CODEX_THREAD_ID':'aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee'})
    pid, master = pty.fork()
    if pid == 0:
        fcntl.ioctl(0,termios.TIOCSWINSZ,struct.pack('HHHH',40,120,0,0))
        os.chdir(workspace)
        os.execve(str(args.binary.resolve()),[str(args.binary.resolve()),'interactive','--settings-id','codex','--config-root',str(config),*([] if args.no_model else ['--model',args.label]),'--prompt','Call Bash once, then finish.'],environment)
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
        with capture_lock:
            persist_requests(captured)
        assert len(captured)==2, f'Expected native Bash call + response, received {len(captured)} requests; artifacts: {root}'
        body=captured[0]
        model=('gpt-5.6-luna' if args.label.startswith('gpt-luna-')
               else 'gpt-5.6-terra' if args.label.startswith('gpt-terra-')
               else 'gpt-6-astra' if args.label.startswith('gpt-astra-')
               else 'gpt-5.6-sol' if args.label.startswith(('gpt-sol-', 'gpt-')) else 'gpt-6-astra')
        assert body['model']==model
        assert body['reasoning']['effort']==effort, body['reasoning']
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
        assert reports and all(report == {'schema_version':1,'token':'local-inventory-token','invocation_uuid':INVOCATION_UUID,'provider_session_id':session} for report in reports),reports
        assert session!=environment['CODEX_THREAD_ID']
        assert calls[0]['owner']==session,calls
        assert_retained_result(calls, captured[1]['input'])
        # Identity originates in native metadata; verify it against the exact
        # rollout ID after binding rather than discovering a latest-session ID.
        matches=[]
        for path in (account/'sessions').rglob('rollout-*.jsonl'):
            first=json.loads(path.open().readline())
            if first.get('payload',{}).get('id')==session:matches.append(path)
        assert len(matches)==1,matches
        assert json.loads(matches[0].open().readline())['payload']['cwd']==str(workspace)
        result={'passed':True,'mode':'native_tui','model':model,'label':args.label,'effort':effort,'no_model':args.no_model,'tools':names,'system_prompt_exact':True,'account_instructions':True,'session_id':session,'metadata_binding':True,'synthetic_snapshot_receipt':True,'remote_tool_ack':'unconfirmed','physical_drain':'unconfirmed','original_user_and_project_mcp_excluded':True,'artifacts':str(root)}
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
        receiver.close()
        (root/'binding-reports.json').write_text(json.dumps(reports,indent=2)+'\n')
        server.shutdown()


if __name__=='__main__':main()

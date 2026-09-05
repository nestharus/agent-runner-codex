#!/usr/bin/env python3
"""Exercise installed Codex through the provider against a loopback Responses stub.
No model request leaves this process's HTTP server. No credentials are copied.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import threading


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=Path('target/debug/agent-runner-codex'))
    parser.add_argument('--label', choices=[prefix+e for prefix in ['gpt-', 'codex-gpt-'] for e in ['low','medium','high','xhigh','max']]+['gpt-luna-low','gpt-luna-max','codex-exec-bench'], default='gpt-high')
    parser.add_argument('--positive-control', action='store_true', help='Remove user-config isolation and prove the injected project MCP appears')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    model = 'gpt-5.6-luna' if args.label == 'codex-exec-bench' or args.label.startswith('gpt-luna-') else 'gpt-6-astra'
    effort = 'low' if args.label == 'codex-exec-bench' else args.label.rsplit('-', 1)[-1]
    route_args = ['-m', model, '-c', 'model_reasoning_effort='+json.dumps(effort)]
    captured = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            captured.append(json.loads(self.rfile.read(int(self.headers['Content-Length']))))
            event = {'type': 'response.completed', 'response': {'id': 'resp_local_inventory', 'object': 'response', 'status': 'completed', 'output': [], 'usage': {'input_tokens': 1, 'output_tokens': 0, 'total_tokens': 1}}}
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            self.wfile.write(('event: response.completed\ndata: ' + json.dumps(event) + '\n\n').encode())
        def log_message(self, *_):
            pass
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='codex-inventory-') as directory:
            root = Path(directory)
            (root / '.codex').mkdir()
            workspace = root / 'project'
            (workspace / '.git').mkdir(parents=True)
            (workspace / '.codex').mkdir()
            (workspace / '.codex/config.toml').write_text(
                '[mcp_servers.project_extra]\ncommand='+json.dumps(shutil.which('bun'))+
                '\nargs='+json.dumps(['--no-install', str(repo/'integrations/codex/agent-bash-mcp.ts')])+
                '\nrequired=true\n[features]\nshell_tool=true\nmulti_agent=true\n')
            trust = 'projects.'+json.dumps(str(workspace))+'.trust_level="trusted"'
            (root / '.codex/config.toml').write_text(trust+'\n')
            config_root = root / 'config'
            install = config_root / 'agent-runner-codex'
            install.mkdir(parents=True)
            wrapper = root / 'native-codex'
            # A test-only executable binding adds a local provider; production
            # model labels and managed tooling arguments are still produced by Rust.
            override = 'model_providers.inventory=' + '{name="inventory",base_url="http://127.0.0.1:' + str(server.server_port) + '",wire_api="responses",requires_openai_auth=false}'
            wrapper.write_text('#!/usr/bin/env python3\nimport os,sys\n' +
                'args = sys.argv[1:]\n' +
                ('args = [a for a in args if a != "--ignore-user-config"]\n' if args.positive_control else '') +
                'if args != ["--version"]: args += ["-c", \'model_provider="inventory"\', "-c", ' + repr(override) + ', "-c", '+repr(trust)+']\n' +
                'os.execv(' + repr(shutil.which('codex')) + ', ["codex", *args])\n')
            wrapper.chmod(0o755)
            runtime = {
                'codex_bin': str(wrapper), 'bun_bin': shutil.which('bun'),
                'bash_mcp_path': str(repo / 'integrations/codex/agent-bash-mcp.ts'),
                'system_prompt_file': str(Path.home() / 'ai/AGENTS.md'),
                'agent_bash_bin': str(Path.home() / '.local/bin/agent-bash'),
                'agent_runner_bin': str(Path.home() / '.local/bin/agents'),
            }
            (install / 'config.toml').write_text('\n'.join(k+'='+json.dumps(v) for k,v in runtime.items())+'\n')
            request = {'contract': 'oulipoly.provider/v1', 'request_id': 'inventory-local', 'provider_instance_id': 'codex',
                'host': {'app': 'inventory-test', 'config_root': str(config_root), 'data_root': str(root/'data'), 'env': {'HOME': str(root)}},
                'params': {'settings_id': 'codex', 'mode': 'arg', 'model': {'name': args.label, 'provider_args': route_args, 'inputs': {'prompt': 'Return an empty completed response.', 'named': {}}},
                    'argv': ['codex','exec','--dangerously-bypass-approvals-and-sandbox',*route_args], 'working_directory': str(workspace), 'env': {}}}
            result = subprocess.run([str(args.binary.resolve()), 'launch'], input=json.dumps(request), text=True, capture_output=True, timeout=45)
            assert result.returncode == 0, result.stdout + result.stderr
            assert len(captured) == 1, f'Expected one local Responses request, got {len(captured)}'
            body = captured[0]
            assert body['model'] == model, body['model']
            wire_effort = effort
            assert body['reasoning']['effort'] == wire_effort, body['reasoning']
            if effort == 'max':
                assert body['reasoning']['context'] == 'all_turns', body['reasoning']
            tools = []
            for item in body.get('input', []):
                if item.get('type') == 'additional_tools':
                    tools.extend(item.get('tools', []))
            tools.extend(body.get('tools', []))
            names = []
            for tool in tools:
                if tool.get('type') == 'namespace':
                    names.extend(tool['name']+'.'+child['name'] for child in tool.get('tools', []))
                else:
                    names.append(tool.get('name', tool.get('type')))
            assert 'mcp__agent_bash.bash' in names, names
            allowed = {'mcp__agent_bash.bash', 'functions.request_user_input', 'functions.list_mcp_resources', 'functions.list_mcp_resource_templates', 'functions.read_mcp_resource'}
            if args.positive_control:
                allowed.add('mcp__project_extra.bash')
                assert 'mcp__project_extra.bash' in names, 'Positive control failed to expose project tool'
            assert set(names) <= allowed, f'Unexpected native tools: {set(names)-allowed}'
            instructions = Path(runtime['system_prompt_file']).read_text().rstrip()
            texts = [''.join(c.get('text','') for c in i.get('content',[]) if isinstance(c,dict)) for i in body.get('input',[]) if i.get('role')=='developer' and isinstance(i.get('content'),list)]
            assert instructions in texts or body.get('instructions','').rstrip()==instructions, 'System instruction source differs'
            print(json.dumps({'passed': True, 'label': args.label, 'model': body.get('model'), 'effort': wire_effort, 'tools': names, 'system_prompt_exact': True}, indent=2))
    finally:
        server.shutdown()

if __name__ == '__main__':
    main()

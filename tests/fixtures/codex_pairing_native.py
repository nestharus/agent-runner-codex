#!/usr/bin/env python3
import json, os, subprocess, sys, uuid, time, pathlib
args = sys.argv[1:]
prompt = sys.stdin.read()
with open(os.environ['CALLS'], 'a') as f:
    f.write(json.dumps({'argv': args, 'prompt': prompt, 'home': os.environ.get('CODEX_HOME'), 'cwd': os.getcwd(), 'pid': os.getpid(), 'pgid': os.getpgrp(), 'sentinel': os.environ.get('SENTINEL'), 'tools': os.environ.get('OULIPOLY_TOOL_MEDIATION_V1')}) + '\n')
if 'resume' in args:
    thread = args[args.index('resume') + 1]
else:
    thread = str(uuid.uuid4())
    sessions = os.path.join(os.environ['CODEX_HOME'], 'sessions')
    os.makedirs(sessions, exist_ok=True)
    with open(os.path.join(sessions, 'rollout-%s.jsonl' % thread), 'a') as rollout:
        rollout.write(json.dumps({'timestamp': '2026-10-06T00:00:00Z', 'type': 'session_meta', 'payload': {'id': thread, 'cwd': os.getcwd()}}) + '\n')
def emit(event):
    print(json.dumps(event), flush=True)
emit({'type': 'thread.started', 'thread_id': thread})
words = prompt.split()
if words and words[0] == 'noconsume':
    sys.exit(4)
emit({'type': 'turn.started'})
if words and words[0] == 'hang':
    child = subprocess.Popen(['sleep', '300'])
    open(os.path.join(words[1], 'descendant.pid'), 'w').write(str(child.pid))
    emit({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'waiting'}})
    child.wait()
    sys.exit(0)
if words and words[0] == 'recordfail':
    root = pathlib.Path(os.environ['PAIRING_PROVIDER_ROOT'])
    deadline = time.monotonic() + 5
    while True:
        records = list(root.glob('**/inputs/*.json'))
        matches = [p for p in records if json.loads(p.read_text()).get('phase') == 'inserted']
        if matches:
            # Obstruct only the synthetic endpoint's final input publication.
            path = matches[0]
            path.rename(path.with_suffix('.before'))
            path.mkdir()
            break
        if time.monotonic() >= deadline:
            sys.exit(98)
        time.sleep(0.01)
if words and words[0] == 'fail':
    emit({'type': 'turn.failed', 'error': {'message': 'fixture failure'}})
    sys.exit(1)
emit({'type': 'item.completed', 'item': {'type': 'agent_message', 'text': 'reply to %s' % prompt.strip()}})
emit({'type': 'turn.completed', 'usage': {}})

#!/usr/bin/env python3
"""Build only the isolated AGE356 test harness; never edit/build the native product.
Public input: codex-rs at rust-v0.153.4 / 3d2ee51ca2d5db578f328aa75e20aa22c0197c9a.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--native-source', required=True, type=Path)
parser.add_argument('--output-dir', required=True, type=Path)
args = parser.parse_args()
native = args.native_source.resolve()
output = args.output_dir.resolve()
if output.is_relative_to(native):
    raise SystemExit('The harness output must be outside the native source')
manifest = tomllib.loads((native / 'Cargo.toml').read_text())
if manifest['workspace']['package']['version'] != '0.153.4':
    raise SystemExit('Expected the pinned public Codex source, not a native upgrade')
output.mkdir(parents=True, exist_ok=True)
(output / 'src').mkdir(exist_ok=True)
shutil.copyfile(Path(__file__).with_name('registration_probe.rs'), output / 'src/main.rs')
text = '[package]\nname="age356-native-probe"\nversion="0.1.0"\nedition="2024"\n[dependencies]\n'
for name, path in [('codex-hooks', 'hooks'), ('codex-config', 'config'), ('codex-protocol', 'protocol'), ('codex-file-system', 'file-system'), ('codex-utils-path-uri', 'utils/path-uri')]:
    text += f'{name}={{path={json.dumps(str(native / path))}}}\n'
for name in ['anyhow', 'futures', 'serde_json', 'tokio', 'toml']:
    declared = manifest['workspace']['dependencies'][name]
    version = declared if isinstance(declared, str) else declared['version']
    features = ',features=["macros","rt-multi-thread","time"]' if name == 'tokio' else ''
    text += f'{name}={{version={json.dumps(version)}{features}}}\n'
text += '\n[patch.crates-io]\n'
for name, patch in manifest['patch']['crates-io'].items():
    text += f'{name}={{' + ','.join(f'{k}={json.dumps(v)}' for k,v in patch.items()) + '}\n'
(output / 'Cargo.toml').write_text(text)
# Preserve upstream dependency selections, not merely the native source version.
# Cargo adds the harness root and prunes unused workspace packages on build.
shutil.copyfile(native / 'Cargo.lock', output / 'Cargo.lock')
# Native source is immutable input. Normal configured dependency acquisition is
# permitted, but the runtime HOME/config/data roots are always private physical dirs.
env = dict(os.environ)
env.update(CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='', CARGO_INCREMENTAL='0', CARGO_TARGET_DIR=str(output / 'target'),
           CARGO_HOME=os.environ.get('CARGO_HOME', str(Path.home() / '.cargo')),
           RUSTUP_HOME=os.environ.get('RUSTUP_HOME', str(Path.home() / '.rustup')))
with tempfile.TemporaryDirectory(prefix='age356-native-build-') as tmp:
    root=Path(tmp)
    for name in ['home','config','data','tmp']:(root/name).mkdir()
    env.update(HOME=str(root/'home'), XDG_CONFIG_HOME=str(root/'config'), XDG_DATA_HOME=str(root/'data'), TMPDIR=str(root/'tmp'))
    subprocess.run(['cargo','build','--manifest-path',str(output/'Cargo.toml')],env=env,check=True,timeout=900)
print(output / 'target/debug/age356-native-probe')

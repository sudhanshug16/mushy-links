#!/usr/bin/env python3
"""Exercise the actual shell wrappers without linking real user devices."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

BIN = Path(os.environ.get('MUSHY_TEST_BIN', 'target/release/mushy')).resolve()
if os.name == 'nt':
    BIN = BIN.with_suffix('.exe')


def invoke(argv, **kwargs):
    result = subprocess.run(argv, capture_output=True, text=True, timeout=30, **kwargs)
    if result.returncode:
        raise AssertionError(f'{argv}:\n{result.stdout}\n{result.stderr}')
    return result.stdout


def posix_shell(shell, tmp):
    if not shutil.which(shell):
        return False
    rc = tmp / (shell + '.rc')
    invoke([str(BIN), 'shell', 'install', shell, '--rc-file', str(rc)])
    before = rc.read_bytes()
    invoke([str(BIN), 'shell', 'install', shell, '--rc-file', str(rc)])
    assert rc.read_bytes() == before
    fake = tmp / 'fake bin'
    fake.mkdir(exist_ok=True)
    log = tmp / (shell + '.jsonl')
    for program, status in [('mushy', 17), ('ln', 9)]:
        file = fake / program
        file.write_text(f'#!{sys.executable}\nimport json,os,sys\nwith open(os.environ["MUSHY_TEST_LOG"],"a") as f: f.write(json.dumps(["{program}",*sys.argv[1:]])+"\\n")\nsys.exit({status})\n')
        file.chmod(0o755)
    env = dict(os.environ, MUSHY_TEST_LOG=str(log), MUSHY_FAKE_BIN=str(fake), MUSHY_TEST_RC=str(rc))
    if shell == 'fish':
        script = '''source "$MUSHY_TEST_RC"
set -gx PATH "$MUSHY_FAKE_BIN" $PATH
ln -m 'peer:/a b' 'local copy'
test $status -eq 17; or exit 1
ln -s 'ordinary source' 'ordinary link'
test $status -eq 9; or exit 2
ln -- -m literal
test $status -eq 9; or exit 3
ln -sm 'peer:/a b' 'local copy'
test $status -eq 17; or exit 4
'''
    else:
        script = '''. "$MUSHY_TEST_RC"
export PATH="$MUSHY_FAKE_BIN:$PATH"
ln -m 'peer:/a b' 'local copy'
[ $? = 17 ] || exit 1
ln -s 'ordinary source' 'ordinary link'
[ $? = 9 ] || exit 2
ln -- -m literal
[ $? = 9 ] || exit 3
ln -sm 'peer:/a b' 'local copy'
[ $? = 17 ] || exit 4
'''
    flags = {'bash': ['--noprofile', '--norc'], 'zsh': ['-f'], 'fish': ['--no-config']}[shell]
    invoke([shell, *flags, '-c', script], env=env)
    assert [json.loads(line) for line in log.read_text().splitlines()] == [
        ['mushy', 'ln', '--', '-m', 'peer:/a b', 'local copy'],
        ['ln', '-s', 'ordinary source', 'ordinary link'],
        ['ln', '--', '-m', 'literal'],
        ['mushy', 'ln', '--', '-sm', 'peer:/a b', 'local copy'],
    ]
    print(f'PASS: {shell} ln -m dispatch, quoted arguments, exit codes, and native ln passthrough', flush=True)
    return True


def powershell(tmp, executable='pwsh'):
    if not shutil.which(executable):
        return False
    rc = tmp / (executable + '.profile.ps1')
    invoke([str(BIN), 'shell', 'install', 'powershell', '--rc-file', str(rc)])
    fake = tmp / ('fake native ' + executable)
    fake.mkdir()
    if os.name == 'nt':
        (fake / 'ln.cmd').write_text('@echo native-ln\n@exit /b 9\n')
    else:
        script = fake / 'ln'
        script.write_text('#!/bin/sh\necho native-ln\nexit 9\n')
        script.chmod(0o755)
    env = dict(os.environ, MUSHY_TEST_RC=str(rc), MUSHY_FAKE_BIN=str(fake))
    script = r'''$ErrorActionPreference = 'Stop'
. $env:MUSHY_TEST_RC
$separator = [IO.Path]::PathSeparator
$env:PATH = $env:MUSHY_FAKE_BIN + $separator + $env:PATH
function global:mushy { $global:mushyCaptured = @($args); $global:LASTEXITCODE = 17 }
ln -m 'peer:/a b' 'local copy'
if ($LASTEXITCODE -ne 17) { throw 'mushy exit code lost' }
if ((ConvertTo-Json -InputObject $global:mushyCaptured -Compress) -ne '["ln","--","-m","peer:/a b","local copy"]') { throw 'mushy arguments changed' }
$native = ln -s source destination
if ($LASTEXITCODE -ne 9 -or $native -ne 'native-ln') { throw "native ln was intercepted: code=$LASTEXITCODE output=$native" }
$native = ln '--' -m literal
if ($LASTEXITCODE -ne 9 -or $native -ne 'native-ln') { throw '-- did not stop option parsing' }
Remove-Item Function:mushy
$help = ln -m --help
if ($LASTEXITCODE -ne 0 -or ($help -join "`n") -notmatch 'mushy link') { throw 'real ln -m help failed' }
'''
    invoke([executable, '-NoProfile', '-NonInteractive', '-Command', script], env=env)
    print(f'PASS: {executable} ln -m dispatch, native ln passthrough, and real CLI help', flush=True)
    return True


def main():
    with tempfile.TemporaryDirectory(prefix='mushy-shell-') as folder:
        tmp = Path(folder)
        tested = []
        if os.name != 'nt':
            tested = [posix_shell(shell, tmp) for shell in ['bash', 'zsh', 'fish']]
        tested.append(powershell(tmp))
        if os.name == 'nt':
            tested.append(powershell(tmp, 'powershell.exe'))
        assert any(tested), 'No supported shell was tested'
        state = tmp / 'unused state'
        invoke([str(BIN), 'shell', 'env', 'powershell'], env=dict(os.environ, MUSHY_STATE_DIR=str(state)))
        assert not state.exists(), 'shell setup should not create sync state'


if __name__ == '__main__':
    main()

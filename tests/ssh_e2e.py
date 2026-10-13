#!/usr/bin/env python3
"""Real OpenSSH end-to-end tests. No changes to ~/.ssh or the system SSH service.
Requires Unix, ssh, ssh-keygen, sshd, and a built debug/release mushy executable.
"""
import getpass
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('MUSHY_TEST_BIN', REPO / 'target/debug/mushy')).resolve()

def run(argv, **kw):
    return subprocess.run([str(x) for x in argv], check=True, capture_output=True, text=True, **kw)

def wait_for(check, detail, timeout=20):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if check():
            return
        time.sleep(.1)
    raise AssertionError(f'Timed out: {detail}')

def main():
    if os.name != 'posix' or not shutil.which('sshd'):
        raise SystemExit('This test requires Unix and OpenSSH server.')
    with tempfile.TemporaryDirectory(prefix='mushy-ssh-') as d:
        base = Path(d)
        remote = base / 'remote folder'; remote.mkdir()
        (remote / 'hello.txt').write_text('seed')
        (remote / 'sub').mkdir()
        (remote / 'sub/data.bin').write_bytes(bytes(range(256)) * 4000)
        (remote / 'alias').symlink_to('hello.txt')
        local = base / 'local folder'
        third = base / 'third folder'
        state_a, state_b, state_c = [base / f'state-{x}' for x in 'abc']
        run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', base / 'host_key'])
        run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', base / 'client_key'])
        shutil.copyfile(base / 'client_key.pub', base / 'authorized_keys')
        with socket.socket() as s:
            s.bind(('127.0.0.1', 0)); port = s.getsockname()[1]
        # ForceCommand avoids depending on the SSH login shell's PATH.
        wrapper = base / 'remote-helper'
        wrapper.write_text(f'#!/bin/sh\nexport MUSHY_STATE_DIR="{state_b}"\nexec "{BIN}" internal serve\n')
        wrapper.chmod(0o700)
        config = base / 'sshd_config'
        config.write_text(f'''Port {port}
ListenAddress 127.0.0.1
HostKey {base / 'host_key'}
PidFile {base / 'sshd.pid'}
AuthorizedKeysFile {base / 'authorized_keys'}
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
AllowUsers {getpass.getuser()}
ForceCommand {wrapper}
LogLevel ERROR
''')
        host_pub = (base / 'host_key.pub').read_text().strip()
        (base / 'known_hosts').write_text(f'[127.0.0.1]:{port} {host_pub}\n')
        client_config = base / 'ssh_config'
        client_config.write_text(f'''Host test-peer
 HostName 127.0.0.1
 Port {port}
 User {getpass.getuser()}
 IdentityFile {base / 'client_key'}
 IdentitiesOnly yes
 UserKnownHostsFile {base / 'known_hosts'}
''')
        log = (base / 'sshd.log').open('w+')
        server = subprocess.Popen([shutil.which('sshd'), '-D', '-e', '-f', str(config)], stderr=log)
        env = {**os.environ, 'MUSHY_STATE_DIR': str(state_a), 'MUSHY_SSH_CONFIG': str(client_config)}
        env_c = {**env, 'MUSHY_STATE_DIR': str(state_c)}
        daemons = []
        servers = [server]
        try:
            time.sleep(.3)
            if server.poll() is not None:
                log.seek(0); raise AssertionError(log.read())
            def mushy(*args, env=env):
                return run([BIN, *args], env=env, timeout=30)
            print('SSH handshake and initial copy', flush=True)
            mushy('doctor', 'test-peer')
            mushy('link', f'test-peer:{remote}', local, '--no-start')
            assert (local / 'hello.txt').read_text() == 'seed'
            assert (local / 'sub/data.bin').read_bytes() == (remote / 'sub/data.bin').read_bytes()
            assert (local / 'alias').is_symlink()
            occupied=base / 'occupied';occupied.mkdir();(occupied/'keep').write_text('untouched')
            rejected=subprocess.run([str(BIN),'link',f'test-peer:{remote}',str(occupied),'--no-start'],env=env,capture_output=True,text=True)
            assert rejected.returncode != 0 and (occupied/'keep').read_text() == 'untouched'
            print('Automatic push and pull', flush=True)
            daemons.append(subprocess.Popen([str(BIN), 'daemon', 'run'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE))
            (local / 'hello.txt').write_text('from local')
            wait_for(lambda: (remote / 'hello.txt').read_text() == 'from local', 'automatic push')
            (remote / 'hello.txt').write_text('from remote')
            wait_for(lambda: (local / 'hello.txt').read_text() == 'from remote', 'automatic pull')
            print('Third device joins the group', flush=True)
            mushy('link', f'test-peer:{remote}', third, '--no-start', env=env_c)
            daemons.append(subprocess.Popen([str(BIN), 'daemon', 'run'], env=env_c, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE))
            (third / 'third.txt').write_text('relayed through B')
            wait_for(lambda: (local / 'third.txt').exists() and (local / 'third.txt').read_text() == 'relayed through B', 'three-device relay')
            print('Cycle between three devices and concurrent edits', flush=True)
            with socket.socket() as sock:
                sock.bind(('127.0.0.1', 0)); port_c = sock.getsockname()[1]
            wrapper_c=base/'third-helper'
            wrapper_c.write_text(f'#!/bin/sh\nexport MUSHY_STATE_DIR="{state_c}"\nexec "{BIN}" internal serve\n')
            wrapper_c.chmod(0o700)
            config_c=base/'sshd_c_config'
            config_c.write_text(config.read_text().replace(f'Port {port}',f'Port {port_c}').replace(str(wrapper),str(wrapper_c)).replace('sshd.pid','sshd_c.pid'))
            with (base/'known_hosts').open('a') as f: f.write(f'[127.0.0.1]:{port_c} {host_pub}\n')
            with client_config.open('a') as f: f.write(f'''Host test-third
 HostName 127.0.0.1
 Port {port_c}
 User {getpass.getuser()}
 IdentityFile {base / 'client_key'}
 IdentitiesOnly yes
 UserKnownHostsFile {base / 'known_hosts'}
''')
            server_c=subprocess.Popen([shutil.which('sshd'),'-D','-e','-f',str(config_c)],stderr=log)
            servers.append(server_c);time.sleep(.3)
            mushy('link',f'test-third:{third}',local,'--no-start')
            mushy('pause',local);mushy('pause',third,env=env_c);time.sleep(2)
            (local/'hello.txt').write_text('offline A edit')
            (third/'hello.txt').write_text('offline C edit wins')
            future=time.time()+2;os.utime(third/'hello.txt',(future,future))
            mushy('resume',local);mushy('resume',third,env=env_c)
            wait_for(lambda: all((p/'hello.txt').read_text()=='offline C edit wins' for p in [local,remote,third]),'cyclic newest-edit-wins convergence')
            print('Network outage and automatic reconnect', flush=True)
            mushy('pause',local);mushy('pause',third,env=env_c);time.sleep(2)
            server.terminate();server.wait(timeout=4);server_c.terminate();server_c.wait(timeout=4)
            mushy('resume',local);mushy('resume',third,env=env_c)
            (local/'offline.txt').write_text('survives disconnection')
            wait_for(lambda: 'offline' in mushy('status',local,'--json').stdout,'offline connection status')
            server=subprocess.Popen([shutil.which('sshd'),'-D','-e','-f',str(config)],stderr=log)
            server_c=subprocess.Popen([shutil.which('sshd'),'-D','-e','-f',str(config_c)],stderr=log)
            servers.extend([server,server_c])
            wait_for(lambda: all((p/'offline.txt').exists() and (p/'offline.txt').read_text()=='survives disconnection' for p in [remote,third]),'automatic reconnect and reconciliation')
            print('Deletion and recovery', flush=True)
            hist = mushy('history', local / 'hello.txt').stdout
            first_version = hist.splitlines()[0].split()[0]
            (remote / 'hello.txt').unlink()
            wait_for(lambda: not (local / 'hello.txt').exists(), 'remote deletion')
            wait_for(lambda: not (third / 'hello.txt').exists(), 'relayed deletion')
            mushy('restore', local / 'hello.txt', '--version', first_version)
            wait_for(lambda: (remote / 'hello.txt').exists() and (third / 'hello.txt').exists(), 'restore propagates')
            print('Pause/resume and growth limit', flush=True)
            mushy('pause', local)
            time.sleep(1)
            (local / 'paused.txt').write_text('held')
            time.sleep(2)
            assert not (remote / 'paused.txt').exists()
            mushy('resume', local)
            wait_for(lambda: (remote / 'paused.txt').exists(), 'resume')
            mushy('config', 'set', 'file_bytes', '8')
            (local / 'oversized.txt').write_text('this is too large for the configured limit')
            wait_for(lambda: 'limit_exceeded' in mushy('status', local, '--json').stdout, 'growth limit')
            assert not (remote / 'oversized.txt').exists()
            (local / 'oversized.txt').unlink()
            mushy('config', 'set', 'file_bytes', str(100 << 20))
            mushy('resume', local)
            print('Single-file link and oversized preflight refusal', flush=True)
            one = base / 'single-remote.txt'; one.write_text('single')
            copy = base / 'single-local.txt'
            mushy('link', f'test-peer:{one}', copy, '--no-start')
            assert copy.read_text() == 'single'
            mushy('config', 'set', 'file_bytes', '4')
            refused = base / 'refused.txt'
            result = subprocess.run([str(BIN), 'link', f'test-peer:{one}', str(refused), '--no-start'], env=env, capture_output=True, text=True)
            assert result.returncode != 0 and 'limit exceeded' in result.stderr
            assert not refused.exists()
            version = mushy('history', copy).stdout.splitlines()[0].split()[0]
            mushy('unlink', copy)
            assert copy.read_text() == 'single'
            assert version in mushy('history', copy).stdout
            recovered = base / 'exported.txt'
            mushy('restore', copy, '--version', version, '--output', recovered)
            assert recovered.read_text() == 'single'
            print('PASS: real SSH initial copy, binary file, symlink, push/pull, three-device cycle, conflicts, reconnect, deletion, restore, pause/resume, limits, unlink/export', flush=True)
        except Exception:
            for p in [state_a, state_b, state_c]:
                for f in p.glob('*.log'):
                    print(f'--- {f.name} ---\n{f.read_text(errors="replace")}', file=sys.stderr)
            log.seek(0); print(log.read(), file=sys.stderr)
            raise
        finally:
            for state in [state_a, state_b, state_c]:
                state.mkdir(exist_ok=True); (state / 'daemon.stop').write_text('stop')
            for p in daemons:
                try: p.wait(timeout=4)
                except subprocess.TimeoutExpired: p.terminate(); p.wait(timeout=4)
            for server in servers:
                if server.poll() is None:
                    server.terminate(); server.wait(timeout=4)
            log.close()

if __name__ == '__main__':
    main()

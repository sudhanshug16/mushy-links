#!/usr/bin/env python3
"""Install actual release binaries from an isolated HTTP fixture and verify failure safety."""
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tarfile
import tempfile
import threading
import io
import urllib.request
import zipfile

REPO = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get('MUSHY_TEST_BIN', REPO / 'target/release/mushy')).resolve()
if os.name == 'nt':
    BIN = BIN.with_suffix('.exe')


class Handler(SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


def main():
    with tempfile.TemporaryDirectory(prefix='mushy-installer-') as folder:
        tmp = Path(folder)
        served = tmp / 'release'
        served.mkdir()
        suffix = '.exe' if os.name == 'nt' else ''
        names = ['mushy', 'mushy-askpass', 'mushy-background']
        if os.name == 'nt':
            target = 'x86_64-pc-windows-msvc'
            asset = served / f'mushy-{target}.zip'
            with zipfile.ZipFile(asset, 'w') as archive:
                for name in names:
                    archive.write(BIN.with_name(name + suffix), name + suffix)
        else:
            cpu = 'aarch64' if platform.machine() in ['aarch64', 'arm64'] else 'x86_64'
            target = cpu + ('-apple-darwin' if platform.system() == 'Darwin' else '-unknown-linux-gnu')
            asset = served / f'mushy-{target}.tar.gz'
            with tarfile.open(asset, 'w:gz') as archive:
                for name in names:
                    archive.add(BIN.with_name(name), './' + name)
        checksums = served / 'SHA256SUMS'
        shutil.copy2(REPO / 'install.sh', served / 'install.sh')
        checksums.write_text(hashlib.sha256(asset.read_bytes()).hexdigest() + '  ' + asset.name + '\n')
        handler = lambda *args, **kwargs: Handler(*args, directory=str(served), **kwargs)
        server = ThreadingHTTPServer(('127.0.0.1', 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        install = tmp / "user's café install with spaces"
        rc = tmp / 'custom profile'
        rc.write_text('# existing user configuration\n')
        state = tmp / 'isolated state'
        env = dict(os.environ, MUSHY_INSTALL_DIR=str(install), MUSHY_RC_FILE=str(rc),
                   MUSHY_RELEASE_BASE_URL=f'http://127.0.0.1:{server.server_port}',
                   MUSHY_STATE_DIR=str(state), MUSHY_SHELL='powershell' if os.name == 'nt' else 'bash')
        if os.name == 'nt':
            # Keep the disposable runner's persistent user PATH unchanged.
            command = ['pwsh', '-NoProfile', '-NonInteractive', '-Command',
                       "$ErrorActionPreference='Stop'; $saved=[Environment]::GetEnvironmentVariable('Path','User'); try { if ($env:MUSHY_TEST_UNINSTALL) { [Environment]::SetEnvironmentVariable('Path',($env:MUSHY_INSTALL_DIR+';'+$saved),'User') }; . $env:MUSHY_TEST_INSTALLER; if ($env:MUSHY_TEST_UNINSTALL -and ([Environment]::GetEnvironmentVariable('Path','User') -split ';') -contains $env:MUSHY_INSTALL_DIR) { throw 'Uninstall retained persistent PATH entry' } } finally { [Environment]::SetEnvironmentVariable('Path',$saved,'User') }"]
            env['MUSHY_TEST_INSTALLER'] = str(REPO / 'install.ps1')
        else:
            command = ['sh', str(REPO / 'install.sh')]
        try:
            executables = ['pwsh', 'powershell.exe'] if os.name == 'nt' else ['sh']
            for executable in executables:
                assert shutil.which(executable), f'{executable} missing from native installer verification'
                command[0] = executable
                for attempt in range(2):
                    print(f'Checking {executable} installer, attempt {attempt + 1}', flush=True)
                    result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
                    assert result.returncode == 0, result.stdout + result.stderr
            for name in names:
                assert (install / (name + suffix)).read_bytes() == BIN.with_name(name + suffix).read_bytes()
            content = rc.read_text()
            assert '# existing user configuration' in content
            assert content.count('# >>> mushy ln >>>') == 1
            assert content.count('# >>> mushy PATH >>>') == 1
            assert Path(str(rc) + '.mushy.bak').read_text() == '# existing user configuration\n'
            before = {file.name: file.read_bytes() for file in install.iterdir() if file.is_file()}
            checksums.write_text('0' * 64 + '  ' + asset.name + '\n')
            failed = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
            assert failed.returncode != 0, 'corrupt archive was accepted'
            assert 'checksum mismatch' in (failed.stdout + failed.stderr).lower()
            assert before == {file.name: file.read_bytes() for file in install.iterdir() if file.is_file()}
            assert rc.read_text() == content
            print(f'PASS: {target} installer, upgrade, quoted paths, profile preservation, and checksum rejection', flush=True)
            checksums.write_text(hashlib.sha256(asset.read_bytes()).hexdigest() + '  ' + asset.name + '\n')
            sentinel = install / 'unrelated-user-file'
            sentinel.write_text('keep this file')
            history = state / 'uninstall-retention-check'
            history.write_text('keep recovery content')
            local_file = tmp / 'local-linked-content'
            local_file.write_bytes(b'keep local files\x00')
            for executable in executables:
                command[0] = executable
                # Check cleanup refuses to erase a malformed custom profile.
                rc.write_text(content + '\n# >>> mushy ln >>>\nunfinished\n')
                if os.name == 'nt':
                    uninstall_command = command
                    uninstall_env = dict(env, MUSHY_TEST_INSTALLER=str(REPO / 'uninstall.ps1'), MUSHY_TEST_UNINSTALL='1')
                else:
                    uninstall_command = ['sh', str(REPO / 'uninstall.sh')]
                    uninstall_env = env
                failed = subprocess.run(uninstall_command, env=uninstall_env, capture_output=True, text=True, timeout=60)
                assert failed.returncode != 0, 'malformed profile was accepted'
                assert (install / ('mushy' + suffix)).exists(), 'failed uninstall removed executable'
                rc.write_text(content + '# user edit after installation\n')
                result = subprocess.run(uninstall_command, env=uninstall_env, capture_output=True, text=True, timeout=60)
                assert result.returncode == 0, result.stdout + result.stderr
                for name in names:
                    assert not (install / (name + suffix)).exists()
                assert sentinel.read_text() == 'keep this file'
                assert history.read_text() == 'keep recovery content'
                assert local_file.read_bytes() == b'keep local files\x00'
                retained = rc.read_text(encoding='utf-8-sig')
                assert '# >>> mushy' not in retained
                assert '# existing user configuration' in retained
                assert '# user edit after installation' in retained
                assert Path(str(rc) + '.mushy.bak').read_text() == '# existing user configuration\n'
                print(f'PASS: {executable} uninstall retains files, history, later profile edits, and shared bin contents', flush=True)
                result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
                assert result.returncode == 0, result.stdout + result.stderr
            # Exercise backward compatibility using the published v0.1.1 binary.
            base = 'https://github.com/sudhanshug16/mushy-links/releases/download/v0.1.1/'
            with urllib.request.urlopen(base + asset.name, timeout=30) as response:
                old_archive = response.read()
            with urllib.request.urlopen(base + 'SHA256SUMS', timeout=30) as response:
                old_sums = response.read().decode()
            expected = [line.split()[0] for line in old_sums.splitlines() if line.split()[-1] == asset.name]
            assert len(expected) == 1 and hashlib.sha256(old_archive).hexdigest() == expected[0]
            if os.name == 'nt':
                with zipfile.ZipFile(io.BytesIO(old_archive)) as archive:
                    old_binary = archive.read('mushy.exe')
            else:
                with tarfile.open(fileobj=io.BytesIO(old_archive), mode='r:gz') as archive:
                    old_binary = archive.extractfile('./mushy').read()
            installed = install / ('mushy' + suffix)
            installed.write_bytes(old_binary)
            checksums.write_text('0' * 64 + '  ' + asset.name + '\n')
            failed = subprocess.run(uninstall_command, env=uninstall_env, capture_output=True, text=True, timeout=60)
            assert failed.returncode != 0
            assert 'checksum mismatch' in (failed.stdout + failed.stderr).lower()
            assert installed.read_bytes() == old_binary
            checksums.write_text(hashlib.sha256(asset.read_bytes()).hexdigest() + '  ' + asset.name + '\n')
            result = subprocess.run(uninstall_command, env=uninstall_env, capture_output=True, text=True, timeout=90)
            assert result.returncode == 0, result.stdout + result.stderr
            assert not installed.exists()
            assert sentinel.exists() and history.exists() and local_file.exists()
            assert '# >>> mushy' not in rc.read_text(encoding='utf-8-sig')
            print('PASS: verified temporary helper uninstalls v0.1.1 and rejects corrupt helper downloads', flush=True)
        finally:
            installed = install / ('mushy' + suffix)
            if installed.exists():
                subprocess.run([str(installed), 'daemon', 'stop'], env=env, capture_output=True, timeout=30)
                import time
                time.sleep(1)
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == '__main__':
    main()

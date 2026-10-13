#!/usr/bin/env python3
"""Native CLI/background smoke test; --startup is for disposable CI runners only."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import time


def run(exe, *args, env, timeout=30):
    print("Checking:", " ".join(args), flush=True)
    return subprocess.run(
        [str(exe), *args], env=env, check=True, capture_output=True,
        text=True, timeout=timeout,
    ).stdout


def check_daemon(exe, env):
    run(exe, "daemon", "start", env=env)
    time.sleep(1)
    # A second foreground daemon must exit because the background daemon owns
    # the OS lock. Without a functioning background launch this times out.
    run(exe, "daemon", "run", env=env, timeout=5)


def check_startup(exe, env):
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("startup verification requires a disposable GitHub Actions runner")
    native = dict(env)
    native.pop("MUSHY_STATE_DIR", None)
    startup = None
    domains = []
    try:
        run(exe, "daemon", "install", env=native)
        if os.name == "nt":
            import winreg
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                               r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
                value, kind = winreg.QueryValueEx(key, "MushyLinks")
            assert kind == winreg.REG_SZ
            assert value == f'"{exe.with_name("mushy-background.exe")}"'
            launcher = exe.with_name("mushy-background.exe")
            subprocess.run([str(launcher)], env=native, check=True, timeout=30)
        elif os.uname().sysname == "Darwin":
            startup = Path.home() / "Library/LaunchAgents/dev.mushy.links.plist"
            with startup.open("rb") as file:
                config = plistlib.load(file)
            assert config["ProgramArguments"] == [str(exe), "daemon", "run"]
            assert config["RunAtLoad"]
            assert config["KeepAlive"] == {"SuccessfulExit": False}
            uid = str(os.getuid())
            domains = [f"gui/{uid}/dev.mushy.links", f"user/{uid}/dev.mushy.links"]
            assert any(subprocess.run(["launchctl", "print", domain],
                                      capture_output=True).returncode == 0 for domain in domains)
            run(exe, "daemon", "run", env=native, timeout=5)
        else:
            config = Path(native.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
            service = config / "systemd/user/mushy.service"
            desktop = config / "autostart/mushy.desktop"
            startup = service if service.exists() else desktop
            text = startup.read_text()
            assert str(exe) in text and "daemon run" in text
            if startup == service:
                subprocess.run(["systemctl", "--user", "is-enabled", "mushy.service"],
                               check=True, capture_output=True)
            else:
                subprocess.run(["desktop-file-validate", str(startup)], check=True)
        check_daemon(exe, native)
        run(exe, "daemon", "uninstall", env=native)
        run(exe, "daemon", "uninstall", env=native)
        if startup:
            assert not startup.exists()
        if os.name == 'nt':
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                               r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
                try:
                    winreg.QueryValueEx(key, 'MushyLinks')
                except FileNotFoundError:
                    pass
                else:
                    raise AssertionError('login registry value survived uninstall')
        elif domains:
            assert all(subprocess.run(['launchctl', 'print', domain], capture_output=True).returncode != 0 for domain in domains)
        print("PASS: native login startup registration and daemon launch", flush=True)
    finally:
        if os.name == "nt":
            import winreg
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER,
                               r"Software\Microsoft\Windows\CurrentVersion\Run",
                               0, winreg.KEY_SET_VALUE) as key:
                try:
                    winreg.DeleteValue(key, "MushyLinks")
                except FileNotFoundError:
                    pass
        elif os.uname().sysname == "Darwin":
            for domain in domains:
                subprocess.run(["launchctl", "bootout", domain], capture_output=True)
        elif startup and startup.name == "mushy.service":
            subprocess.run(["systemctl", "--user", "disable", "--now", "mushy.service"],
                           capture_output=True)
        run(exe, "daemon", "stop", env=native)
        if startup:
            startup.unlink(missing_ok=True)
        time.sleep(1)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--startup", action="store_true")
    args = parser.parse_args()
    binary = Path(os.environ.get("MUSHY_TEST_BIN", "target/release/mushy")).resolve()
    suffix = ".exe" if os.name == "nt" else ""
    if suffix and binary.suffix != suffix:
        binary = binary.with_suffix(suffix)
    with tempfile.TemporaryDirectory(prefix="mushy-native-") as tmp:
        install = Path(tmp) / "install with spaces"
        install.mkdir()
        for name in ["mushy", "mushy-askpass", "mushy-background"]:
            shutil.copy2(binary.with_name(name + suffix), install / (name + suffix))
        exe = install / ("mushy" + suffix)
        env = dict(os.environ, MUSHY_STATE_DIR=str(Path(tmp) / "isolated state"))
        try:
            assert "Diagnostics passed" in run(exe, "doctor", env=env)
            assert json.loads(run(exe, "config", "show", env=env))["bytes"] == 1 << 30
            run(exe, "config", "set", "file_bytes", "4096", env=env)
            assert json.loads(run(exe, "config", "show", env=env))["file_bytes"] == 4096
            run(exe, "list", env=env)
            check_daemon(exe, env)
            launcher = install / ("mushy-background" + suffix)
            subprocess.run([str(launcher)], env=env, check=True, timeout=30)
            print("PASS: native CLI, config, and background helpers from a path with spaces", flush=True)
        finally:
            run(exe, "daemon", "uninstall", env=env)
        if args.startup:
            check_startup(exe, env)


if __name__ == "__main__":
    main()

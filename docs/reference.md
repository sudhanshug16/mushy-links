# mushy reference

[Back to the quick start](../README.md)

Link a file or folder on another device, then edit the local copy normally.
`mushy` automatically copies changes in both directions over SSH, including
deletions, and keeps recoverable versions. Files remain usable while offline.

```sh
mushy link devbox:/home/me/project ./project
mushy status
```

This is an early implementation of automatic synchronization. Each device has
its own copy, and those copies converge when their connections are available.

## Install

Download a binary archive for your platform from the
[GitHub releases](https://github.com/sudhanshug16/mushy-links/releases).
Archives include all three binaries, this README, and the license. Extract them
into the same directory and add that directory to your PATH on each device.
`SHA256SUMS` accompanies each release. The packaged targets are Linux x86_64 and ARM64,
macOS Apple Silicon and Intel, and Windows x86_64. Linux binaries require glibc
2.35 or newer (Ubuntu 22.04 or newer); use a source build on older distributions. macOS binaries are not
Developer ID signed or notarized. Windows builds statically link the C runtime
so a separate Visual C++ runtime installation is not required.

Install on every device that participates. Runtime requirements are OpenSSH and
the binaries included with mushy; SSHFS, rsync, FUSE, Python, and a server daemon
are not required. The receiving device must already accept SSH connections.

From this repository, with a Rust toolchain and a native C compiler:

```sh
cargo install --path crates/mushy --locked
```

This installs `mushy`, `mushy-askpass`, and `mushy-background`. Keep the helpers
beside `mushy`, and make `mushy` available in the remote noninteractive shell's
PATH. Both peers must use compatible protocol versions.

Before linking, verify ordinary SSH access and host trust:

```sh
ssh devbox exit
mushy doctor devbox
```

Mushy uses the system SSH client and your existing SSH config, host aliases,
keys, and agent. It never approves unknown or changed host keys in the background.
For encrypted private keys, unlock the key in your SSH agent.

Password-based connections require an unlocked OS secure credential store:
macOS Keychain, Windows Credential Manager, or Linux Secret Service. There is no
plaintext or memory-only password fallback.

```sh
mushy auth login devbox
mushy auth forget devbox
```

The askpass helper reads the saved password from the OS store. Passwords are
never passed in arguments, environment variables, SQLite, or logs. Interactive
MFA and repeated challenge-response prompts are outside this version.
Credential writes and reads use the same helper executable so macOS Keychain
can authorize that executable for background access. After upgrading or moving
the helper, the OS may require authorizing it again.

## Link files, folders, and multiple devices

The remote path seeds a new local destination. A new destination must be absent
or empty, and its parent must exist. Paths with spaces are supported. A remote
`~/` prefix expands to that account's home directory.

```sh
mushy link devbox:/home/me/notes ./notes
mushy link devbox:/home/me/config.json ./config.json
mushy link --host desktop --remote-path 'C:\Users\me\notes' ./notes
mushy link devbox:/home/me/project ./project --exclude 'target/**' --exclude '.git'
```

Exclusions apply before scanning, transferring, and limit accounting. They are
fixed for a sync group; new members inherit them. Exclude a directory by name
(e.g. `.git`) to omit the directory itself and all descendants. A pattern such
as `target/**` excludes descendants but can retain an empty `target` directory.
No user folders are silently excluded.

A second device joins the same group by linking to an existing member:

```sh
# On a third device:
mushy link devbox:/home/me/notes ./notes
```

To add another edge, run `link` with an already linked local root and a remote
root in the same group. The remote root may also be a new, empty destination:

```sh
mushy link third-device:/home/me/notes ./notes
```

Chains, stars, diamonds, and cycles are supported. Each configured edge needs
SSH access in the direction it was created, but changes move in both directions
through that connection. Peers do not discover new hosts, forward credentials,
or require every pair to connect. Updates wait if all connecting paths are down.

Each device has one root per group. Overlapping roots and automatic merges of
different groups are rejected. Clone files freely, but do not copy mushy's state
directory to another installation: each installation needs a distinct device ID.

The first successful link registers per-user login startup and starts the daemon.
Linux uses a systemd user service, with XDG desktop autostart as a fallback;
macOS uses a LaunchAgent; Windows uses the current user's Run registry key and a
windowless launcher. No administrator service is installed.

```sh
mushy pause ./notes
mushy resume ./notes
mushy daemon stop
mushy daemon start
mushy daemon run       # foreground operation and diagnostics
mushy daemon install   # repair login startup registration
```

`--no-start` on `link` performs initial sync and registers the edge without
installing startup or starting the daemon. A daemon already running will pick
up that registered edge. `daemon stop` stops the current process; login startup
remains registered. Login services may restart a foreground process terminated
outside the stop command.

## Conflicts, deletion, and recovery

Causally later edits supersede earlier ones. Concurrent edits use the original
file modification time, then device ID and change ID to break ties deterministically.
Relayed copies retain their original edit identity. File modification times are
preserved during transfer. `mushy doctor HOST` reports clock difference; incorrect
clocks can select the wrong concurrent winner. A deletion's timestamp is when
mushy first observes the missing entry, including after reconnect.

Deletions propagate automatically. Deleted content, overwritten content, and
local conflict losers remain recoverable for seven days by default. Each device
stores the versions it has held; a losing version need not be available on every
peer. History marks content that is unavailable or expired.

```sh
mushy history ./notes/readme.txt
mushy restore ./notes/readme.txt --version DEVICE_ID:SEQUENCE
```

Restoration is a new edit and synchronizes normally. Renames transfer as a delete
and a create. A surviving concurrent child keeps its parent directory alive.
Excluded files prevent removal of a directory containing them. File/directory
structural conflicts pause progress and require renaming or removing the obstruction.

```sh
mushy unlink ./notes
mushy history ./notes/readme.txt
mushy restore ./notes/readme.txt --version DEVICE_ID:SEQUENCE --output ./recovered.txt
```

Unlink detaches this device's root and retains ordinary files and recovery history.
Other devices continue syncing. `--output` exports a regular-file version to a new,
absent path, including after unlink, without changing any sync group.

## Workload limits

Links are refused when a bounded preflight scan cannot verify them. Limits are
checked on each device before initial transfer and again as a group changes.
When a limit is exceeded later, that group's synchronization reports
`limit_exceeded` and retries; it never deletes excess files or silently syncs only
part of the tree. Reduce the workload or raise the limit and resume the root.

| Limit | Default |
| --- | ---: |
| Bytes per root (`bytes`) | 1 GiB |
| Files/symlinks per root (`files`) | 10,000 |
| Directories per root (`directories`) | 1,000 |
| Bytes per file (`file_bytes`) | 100 MiB |
| Bytes per device (`device_bytes`) | 4 GiB |
| Files/symlinks per device (`device_files`) | 40,000 |
| Directories per device (`device_directories`) | 4,000 |
| Serialized version metadata (`metadata_bytes`) | 256 MiB |
| Recovery content per device (`recovery_bytes`) | 2 GiB |
| Recovery retention (`recovery_days`) | 7 days |
| Scan budget (`scan_seconds`) | 60 seconds |

```sh
mushy config show
mushy config set bytes 2147483648
mushy config set file_bytes 209715200
mushy config set recovery_bytes 4294967296
```

Values are positive integers. Byte limits use bytes, not MiB. Raising one device's
limits cannot override another's. A root counts once in device totals regardless
of peer count. Recovery storage is deduplicated by content hash. If unexpired
history fills its budget, destructive updates wait rather than evict it early.
Active content is additionally cached; keep room for active copies, their object
cache, recovery content, and staged replacements. Deletion metadata remains after
recovery bytes expire, preventing old peers from resurrecting removed files.

## Filesystem behavior and state

Regular files, directories, and inside-root relative symlinks are supported.
Symlinks are not traversed during scans. Absolute/escaping links and special files
are rejected. Windows needs symlink creation privileges (typically Developer Mode)
for groups containing symlinks. A single-file root cannot itself be a symlink.

For portability, names must fit a 240-byte relative path budget and avoid Windows
reserved names and case collisions. Wire names use Unicode NFC; macOS filesystem
normalization is converted at the boundary. On Linux/Windows, decomposed Unicode
names must be renamed to NFC. Ownership, ACLs, extended attributes, and hard-link
relationships are not synchronized. Executable status is preserved between Unix
peers. Newly written Unix copies are private to the user (0600 or 0700).

Watchers trigger scans; periodic 60-second scans catch missed notifications.
Connections retry with bounded exponential backoff. Whole changed files stream
in bounded chunks; delta transfer is not implemented. Independent connections
run in worker threads, with serialized local filesystem/state commits. SQLite
WAL, operation locks, hash verification, staging files, and a pending-operation
journal protect against partial downloads and interrupted commits. Edits detected
before replacement trigger a rescan. There is no universal filesystem compare-and-swap
against arbitrary external writers; applications actively writing a file during
replacement can still race. This version does not provide transactional database
sync or distributed file locking.

Directory identity is pinned on Unix and Windows. Replacing a directory root,
its mount, or the parent directory of a single-file root pauses synchronization
instead of publishing mass deletions. Windows requires a filesystem that can
report a stable volume and file ID; inability to query that ID blocks the scan.

```sh
mushy list
mushy status ./notes --json
mushy doctor
```

`doctor` shows the state directory. State uses standard per-user app data locations:
`~/.local/share/mushy-links` on Linux (subject to XDG settings),
`~/Library/Application Support/dev.mushy.mushy-links` on macOS, and the user's
local AppData on Windows. It contains SQLite metadata, content objects, daemon
logs, and bounded SSH diagnostic logs. Credentials are kept separately in the OS
store. The CLI and daemon coordinate through SQLite and OS file locks; helpers
use the same state without a local TCP service.

For isolated testing, `MUSHY_STATE_DIR` selects a separate state directory and
suppresses login startup registration. `MUSHY_SSH_CONFIG` selects an SSH config
file. Do not change these between normal CLI and daemon invocations.

## Development and verification

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
python3 tests/native_smoke.py
python3 tests/ssh_e2e.py
```

The real SSH test uses temporary keys, two loopback SSH servers, and isolated
state directories. It leaves the user's SSH config, known hosts, startup settings,
and system SSH service alone. It requires Python and OpenSSH server for testing
only. To test release binaries, set `MUSHY_TEST_BIN=target/release/mushy`.

CI runs tests and builds on both Linux architectures, both macOS architectures, and Windows. Native
checks include real OS credential-store write/read/delete operations, askpass,
background helpers, and login startup registration. `native_smoke.py --startup`
is restricted to disposable GitHub Actions runners because it registers startup
in the runner's user profile. Windows additionally tests file and directory
symlink synchronization. The live SSH mesh integration runs on Linux.

The credential integration test is ignored in an ordinary `cargo test`; run
`bash tests/secure_store_linux.sh` to provision a private Linux D-Bus session and
temporary secure store, or run `cargo test -p mushy-links --test credentials
--locked -- --ignored` with an unlocked native store. Tests use synthetic passwords
and remove their credentials afterward.

Pushing a `vVERSION` tag runs the same native CI checks, packages the binaries,
includes the installers, generates SHA-256 checksums, and publishes a GitHub release only after all jobs
pass. The tag must match the Cargo workspace version, and release notes must
exist at `docs/releases/vVERSION.md`.

The core crate contains the deterministic version model, bounded scanner,
SQLite store, recovery journal, and filesystem application engine. The CLI crate
contains commands, the background runtime, SSH protocol adapter, credential helper,
and Windows background launcher.

## The ln -m shell shortcut

The installer adds a shell function named `ln`. It sends `ln -m HOST:/path LOCAL`
to `mushy link`; every other `ln` invocation is passed to the native executable
with its arguments intact. `ln -sm` is also accepted. `--` ends shortcut flag
parsing, so a literal filename `-m` can still be passed to native `ln`.
Other native `ln` options, such as force replacement, are not accepted with `-m`.
Use `mushy link` for advanced sync options such as `--exclude`.

```sh
mushy shell install            # Detect your shell and install the shortcut
mushy shell install zsh        # Choose a different shell
mushy shell env bash           # Print setup for a custom configuration
mushy shell install bash --rc-file ./custom-bashrc
```

Bash setup uses `.bashrc` and the existing login profile. Zsh uses `.zshenv`
for PATH and `.zshrc` for the shortcut, respecting an exported `ZDOTDIR`.
Fish uses `config.fish` under `XDG_CONFIG_HOME`. The PowerShell installer uses
`$PROFILE.CurrentUserAllHosts` and adds the installation to the user PATH so SSH
can find it. PowerShell profiles need to be enabled by your existing execution
policy. The installer does not change that policy.

The installer preserves existing profile content and creates a `.mushy.bak`
backup before the first modification. Reinstalling replaces its marked blocks
without duplicating them. PATH setup goes before interactive-only guards so
remote SSH sessions can find mushy. The shortcut is loaded after existing aliases.
Profile symlinks are followed without replacing the symlink itself.

A shell function is available in shells that load the profile. Scripts and
shells started without configuration can use `mushy link` or `mushy ln -- -m`
directly. Piping the Unix installer into `sh` cannot alter the parent terminal's
functions, so open a new terminal afterward. For Bash/Zsh, you can also run
`eval "$(mushy shell env bash)"` (choose the matching shell).
On Windows, `ln -m` works in PowerShell even if native `ln` is absent. Other
`ln` commands require a native executable, such as the one supplied by Git Bash;
PowerShell's built-in local symlink command is `New-Item -ItemType SymbolicLink`.
PowerShell consumes an unquoted `--` before calling functions; quote it as `'--'`
when passing a literal filename `-m` to native `ln`, or use `./-m` as the filename.

## Installer options

The one-line installers verify each archive against its release's `SHA256SUMS`,
check the binary version, install all three executables together, and configure
the shell. An existing installation is stopped for replacement and restarted.
Unix installs to `~/.local/bin`; Windows installs to `%LOCALAPPDATA%\mushy\bin`.
Windows x64 binaries can run under x64 emulation on Windows 11 ARM.
Run the same installer again to update to the newest released version.

Optional environment variables support custom installations:

- `MUSHY_INSTALL_DIR`: choose a binary directory.
- `MUSHY_SHELL`: choose `bash`, `zsh`, `fish`, or `powershell`.
- `MUSHY_RC_FILE`: choose a custom shell profile.
- `MUSHY_NO_SHELL=1`: install only the binaries.
- `MUSHY_VERSION`: select a compatible release explicitly.

The default installer is attached to its own release and selects that same
version's binaries. `MUSHY_RELEASE_BASE_URL` can point to a private mirror or a
local fixture for testing. The installer requires OpenSSH and standard download,
archive, and checksum tools. It never installs an SSH server, changes host trust,
or asks for administrator privileges.

Installer verification uses isolated HTTP fixtures and temporary profiles:

```sh
python3 tests/shell_smoke.py
python3 tests/installers_smoke.py
```

CI executes the wrappers in Bash, Zsh, Fish, and PowerShell, verifies argument
preservation and native `ln` passthrough, and checks install/upgrade and checksum
rejection. The real SSH test also exercises initial linking through `ln -m`.

## Uninstall

The README's uninstall command removes this device's installation. It disables
login startup before waiting for the daemon to exit, strips only mushy's marked
shell blocks, and removes the three executables. It never recursively deletes
your installation directory or modifies linked files. Profile backups remain.
Windows also removes the installation directory from the persistent user PATH.
For releases before v0.1.2, the scripts download a temporary checksum-verified
cleanup helper; they do not upgrade the installation. The component commands
below are available starting with v0.1.2.

By default, shell cleanup checks Bash's `.bashrc` and all three login profiles,
Zsh's `.zshenv`/`.zshrc`, and Fish's `config.fish`. On Windows it also checks both
PowerShell and Windows PowerShell's current-user-all-hosts profiles. Missing
profiles and profiles without mushy blocks are left untouched. `ZDOTDIR` and
`XDG_CONFIG_HOME` select the same locations as during installation. Custom shell
setup outside these files must be removed where you added it.

For custom installations, set `MUSHY_INSTALL_DIR` and/or `MUSHY_RC_FILE` to the
same values used when installing; `MUSHY_SHELL` selects the custom profile's
shell. For example, in Bash/Zsh:

```sh
export MUSHY_INSTALL_DIR="$HOME/tools/mushy"
export MUSHY_RC_FILE="$HOME/custom-bashrc"
export MUSHY_SHELL=bash
curl -fsSL https://github.com/sudhanshug16/mushy-links/releases/latest/download/uninstall.sh | sh
```

You can also remove components separately, while mushy is still installed:

```sh
mushy shell uninstall                   # Remove all default shell setup
mushy shell uninstall zsh               # Remove only Zsh setup
mushy shell uninstall bash --rc-file ./custom-bashrc
mushy daemon uninstall                  # Remove login startup and stop syncing
```

Sync registrations, local files, recovery history, and OS-stored SSH passwords
remain. After reinstalling, `mushy daemon install` can resume the registered links.
To detach a link permanently,
run `mushy unlink LOCAL` before uninstalling. To forget a saved password, run
`mushy auth forget HOST` before removing the credential helper. Other devices are
unaffected. Uninstall never sends file deletions to peers.

Like startup installation, `mushy daemon uninstall` skips real login configuration
when `MUSHY_STATE_DIR` is set and stops only that isolated state's daemon.

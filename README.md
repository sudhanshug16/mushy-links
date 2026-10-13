# mushy

**Soft links across devices.**

```sh
ln -m devbox:~/notes ./notes
```

Make a file or folder on another device available at a local path. Open it in
any app, edit it normally, and mushy keeps both devices up to date over SSH.

Under the hood, mushy keeps a local copy, so your files also work offline.
Edits and file deletions sync automatically when the devices reconnect.

## Install

Run this on **both devices**. It picks the right download, installs mushy and
its helpers, and sets up `ln -m` in your shell. No administrator access needed.

```sh
curl -fsSL https://github.com/sudhanshug16/mushy-links/releases/latest/download/install.sh | sh
```

Open a new terminal after installing. Bash, Zsh, and Fish are supported.
The installer uses your login shell; to set up another installed shell, run
`mushy shell install zsh` (or `bash` or `fish`).

<details>
<summary>Windows — PowerShell</summary>

Run this in PowerShell. It installs mushy and makes `ln -m` available immediately
and in future PowerShell sessions.

```powershell
irm https://github.com/sudhanshug16/mushy-links/releases/latest/download/install.ps1 | iex
```

</details>

## Make a link

The other device needs to accept SSH connections. Connect once with ordinary
SSH to check access and trust its host key, then make your link:

```sh
ssh devbox exit
ln -m devbox:~/notes ./notes
```

The new local destination must be absent or empty. Link a single file the same way:

```sh
ln -m devbox:~/settings.json ./settings.json
```

That's it. Sync runs in the background and starts again when you log in.
Run the same link command on a third device to share the same files there too.

`-m` is the mushy shortcut. Ordinary `ln` commands, including `ln -s`, still use
your system's `ln`. You can also use `mushy link HOST:/path LOCAL` directly.

## Everyday commands

```sh
mushy status                # See whether your links are up to date
mushy pause ./notes         # Temporarily stop syncing this link
mushy resume ./notes        # Start syncing it again
mushy history ./notes/a.txt # Find recoverable versions
mushy unlink ./notes        # Detach this device and keep its files
```

**Deleting a synced file deletes it on the other devices too.** Use `mushy unlink`
to detach a link. Overwritten and deleted content stays recoverable for seven days
by default. If devices edit the same file while disconnected, the newer edit wins
and losing content stays recoverable.

SSH keys and agents work as usual. To use an SSH password, run
`mushy auth login devbox`; the password is saved in your OS secure credential store.

For limits, recovery commands, shell setup, manual installation, and platform
restrictions, see the [reference](docs/reference.md).

## Uninstall

Run this on each device you want to stop syncing:

```sh
curl -fsSL https://github.com/sudhanshug16/mushy-links/releases/latest/download/uninstall.sh | sh
```

<details>
<summary>Windows — PowerShell</summary>

```powershell
irm https://github.com/sudhanshug16/mushy-links/releases/latest/download/uninstall.ps1 | iex
```

</details>

This removes login startup, stops syncing, removes the `ln -m` shell setup, and
deletes mushy's executables. Open a new terminal afterward. **Your local linked
files and recovery history stay on disk.** Other devices keep their links.

Saved SSH passwords stay in your OS credential store. To remove one, run
`mushy auth forget HOST` before uninstalling. For custom installation paths and
profiles, see [uninstall options](docs/reference.md#uninstall).

#!/bin/sh
# Install mushy and the ln -m shortcut without administrator privileges.
set -eu
MUSHY_VERSION=${MUSHY_VERSION:-v0.1.1}
release_base=${MUSHY_RELEASE_BASE_URL:-https://github.com/sudhanshug16/mushy-links/releases/download/$MUSHY_VERSION}
bin_dir=${MUSHY_INSTALL_DIR:-"$HOME/.local/bin"}
fail() { printf 'mushy: %s\n' "$*" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || fail 'curl is required.'

system=$(uname -s)
arch=$(uname -m)
case "$system" in
    MINGW*|MSYS*|CYGWIN*)
        command -v powershell.exe >/dev/null 2>&1 || fail 'Use the PowerShell installer on Windows.'
        export MUSHY_VERSION
        export MUSHY_SHELL="${MUSHY_SHELL:-bash}"
        export MUSHY_RELEASE_BASE_URL="$release_base"
        # Native Windows installation also makes mushy available to SSH.
        # shellcheck disable=SC2016 # PowerShell, rather than sh, expands $env.
        powershell.exe -NoProfile -Command 'Invoke-Expression (Invoke-WebRequest -UseBasicParsing ($env:MUSHY_RELEASE_BASE_URL + "/install.ps1")).Content'
        exit $?
        ;;
    Linux)
        case "$arch" in
            x86_64|amd64) target=x86_64-unknown-linux-gnu ;;
            aarch64|arm64) target=aarch64-unknown-linux-gnu ;;
            *) fail "No release is available for Linux $arch." ;;
        esac
        ;;
    Darwin)
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then arch=arm64; fi
        case "$arch" in
            x86_64|amd64) target=x86_64-apple-darwin ;;
            arm64|aarch64) target=aarch64-apple-darwin ;;
            *) fail "No release is available for macOS $arch." ;;
        esac
        ;;
    *) fail "No release is available for $system $arch." ;;
esac
command -v ssh >/dev/null 2>&1 || fail 'OpenSSH is required. Install the SSH client, then run this installer again.'
command -v tar >/dev/null 2>&1 || fail 'tar is required.'
if command -v sha256sum >/dev/null 2>&1; then
    checksum=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    checksum=shasum
else
    fail 'sha256sum or shasum is required to verify the download.'
fi
selected_shell=${MUSHY_SHELL:-${SHELL:-bash}}
selected_shell=${selected_shell##*/}
case "$selected_shell" in bash|zsh|fish) ;; *)
    [ "${MUSHY_NO_SHELL:-0}" = 1 ] || fail 'Choose MUSHY_SHELL=bash, zsh, or fish, or set MUSHY_NO_SHELL=1.' ;;
esac

tmp=$(mktemp -d)
stage_dir=
trap 'rm -rf "$tmp"; if [ -n "$stage_dir" ]; then rm -rf "$stage_dir"; fi' EXIT HUP INT TERM
asset=mushy-$target.tar.gz
printf 'Installing mushy %s for %s…\n' "$MUSHY_VERSION" "$target"
curl --fail --silent --show-error --location --retry 3 "$release_base/$asset" -o "$tmp/$asset"
curl --fail --silent --show-error --location --retry 3 "$release_base/SHA256SUMS" -o "$tmp/SHA256SUMS"
expected=$(awk -v asset="$asset" '$2 == asset { print $1 }' "$tmp/SHA256SUMS")
[ "${#expected}" = 64 ] || fail 'The release checksum is missing or invalid.'
case "$expected" in *[!0-9a-f]*) fail 'The release checksum is invalid.' ;; esac
if [ "$checksum" = sha256sum ]; then actual=$(sha256sum "$tmp/$asset"); else actual=$(shasum -a 256 "$tmp/$asset"); fi
actual=${actual%% *}
actual=${actual#\\}
[ "$actual" = "$expected" ] || fail 'Download checksum mismatch; nothing was installed.'
mkdir "$tmp/package"
# Extract only the expected executable names, never arbitrary archive paths.
tar -xzf "$tmp/$asset" -C "$tmp/package" ./mushy ./mushy-askpass ./mushy-background
for name in mushy mushy-askpass mushy-background; do
    if [ ! -f "$tmp/package/$name" ] || [ -L "$tmp/package/$name" ]; then
        fail "The release is missing $name."
    fi
    chmod 755 "$tmp/package/$name"
done
installed_version=$("$tmp/package/mushy" --version) || fail 'This build cannot run here. Linux releases require glibc 2.35 or newer.'
[ "$installed_version" = "mushy ${MUSHY_VERSION#v}" ] || fail 'Release version mismatch; nothing was installed.'
mkdir -p "$bin_dir"
# Resolve relative paths before generating persistent shell setup.
bin_dir=$(cd "$bin_dir" && pwd -P)
for name in mushy mushy-askpass mushy-background; do
    [ ! -e "$bin_dir/$name" ] || [ -f "$bin_dir/$name" ] || fail "Cannot replace $bin_dir/$name: it is not a regular file."
done
updating=0
if [ -x "$bin_dir/mushy" ]; then
    "$bin_dir/mushy" daemon stop >/dev/null
    updating=1
    sleep 1
fi
stage_dir=$(mktemp -d "$bin_dir/.mushy-install.XXXXXX")
for name in mushy mushy-askpass mushy-background; do
    # Prepare the full set before replacing any installed executable.
    cp "$tmp/package/$name" "$stage_dir/$name"
    chmod 755 "$stage_dir/$name"
done
for name in mushy mushy-askpass mushy-background; do mv -f "$stage_dir/$name" "$bin_dir/$name"; done
if [ "${MUSHY_NO_SHELL:-0}" != 1 ]; then
    if [ -n "${MUSHY_RC_FILE:-}" ]; then
        "$bin_dir/mushy" shell install "$selected_shell" --rc-file "$MUSHY_RC_FILE"
    else
        "$bin_dir/mushy" shell install "$selected_shell"
    fi
fi
if [ "$updating" = 1 ]; then "$bin_dir/mushy" daemon start; fi
printf '\nInstalled mushy in %s\nOpen a new terminal, then link a remote folder:\n\n  ln -m devbox:/home/me/notes ./notes\n\nRun this same installer on the other device too.\n' "$bin_dir"

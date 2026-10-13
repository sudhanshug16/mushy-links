#!/bin/sh
# Remove mushy without deleting linked files, history, or user configuration.
set -eu
bin_dir=${MUSHY_INSTALL_DIR:-"$HOME/.local/bin"}
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*)
        export MUSHY_SHELL="${MUSHY_SHELL:-bash}"
        # shellcheck disable=SC2016 # PowerShell expands the environment variable.
        powershell.exe -NoProfile -Command 'Invoke-Expression (Invoke-WebRequest -UseBasicParsing "https://github.com/sudhanshug16/mushy-links/releases/latest/download/uninstall.ps1").Content'
        exit $?
        ;;
esac
if [ ! -x "$bin_dir/mushy" ]; then
    printf 'mushy: No executable at %s/mushy. Set MUSHY_INSTALL_DIR for a custom installation.\n' "$bin_dir" >&2
    exit 1
fi
helper="$bin_dir/mushy"
# Older releases do not have cleanup commands. Use a verified temporary helper
# without upgrading the installation or changing its shell configuration.
if ! "$helper" daemon --help | awk '/uninstall/ { found=1 } END { exit !found }'; then
    printf 'Fetching a cleanup helper for this older installation…\n'
    tmp=$(mktemp -d)
    trap 'rm -f -- "$tmp/mushy" "$tmp/mushy-askpass" "$tmp/mushy-background" "$tmp/install.sh"; rmdir "$tmp"' EXIT HUP INT TERM
    release_base=${MUSHY_RELEASE_BASE_URL:-https://github.com/sudhanshug16/mushy-links/releases/download/v0.1.2}
    curl -fsSL "$release_base/install.sh" -o "$tmp/install.sh"
    MUSHY_INSTALL_DIR="$tmp" MUSHY_NO_SHELL=1 MUSHY_VERSION=v0.1.2 sh "$tmp/install.sh"
    helper="$tmp/mushy"
fi
"$helper" daemon uninstall
if [ -n "${MUSHY_RC_FILE:-}" ]; then
    selected_shell=${MUSHY_SHELL:-${SHELL:-bash}}
    selected_shell=${selected_shell##*/}
    "$helper" shell uninstall "$selected_shell" --rc-file "$MUSHY_RC_FILE"
else
    "$helper" shell uninstall
fi
# Delete only the three installed executables; shared bin directories stay intact.
for name in mushy mushy-askpass mushy-background; do
    if [ -f "$bin_dir/$name" ] || [ -L "$bin_dir/$name" ]; then
        rm -- "$bin_dir/$name"
    fi
done
printf 'Uninstalled mushy. Linked files, sync state, recovery history, and saved passwords were kept.\nOpen a new terminal to restore ordinary ln.\n'

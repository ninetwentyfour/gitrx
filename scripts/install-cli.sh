#!/bin/sh
# install-cli.sh - install (or uninstall) the `gitrx` launcher onto PATH.
#
# Copies scripts/gitrx to the first writable of:
#   /opt/homebrew/bin   (Apple Silicon Homebrew)
#   /usr/local/bin      (Intel Homebrew / system-local)
# If neither is writable, retries /usr/local/bin via sudo.
#
# The script is COPIED (not symlinked) so the project directory can move or be
# deleted without breaking the installed command.
#
# Usage:
#   scripts/install-cli.sh              Install gitrx
#   scripts/install-cli.sh --uninstall  Remove gitrx from the install dirs

set -eu

CMD="gitrx"

script_dir=$(cd "$(dirname "$0")" 2>/dev/null && pwd -P) || {
	printf 'install-cli: cannot determine script directory\n' >&2
	exit 1
}
SRC="$script_dir/$CMD"

# Install target search path. Overridable (space-separated) for testing.
DIRS="${GITRX_INSTALL_DIRS:-/opt/homebrew/bin /usr/local/bin}"

uninstall() {
	removed=0
	for d in $DIRS; do
		dest="$d/$CMD"
		if [ -e "$dest" ]; then
			if [ -w "$d" ]; then
				rm -f "$dest" && printf 'Removed %s\n' "$dest"
			else
				printf 'Need elevated permissions to remove %s\n' "$dest"
				sudo rm -f "$dest" && printf 'Removed %s\n' "$dest"
			fi
			removed=1
		fi
	done
	[ "$removed" -eq 1 ] || printf '%s was not found in: %s\n' "$CMD" "$DIRS"
}

install() {
	[ -f "$SRC" ] || {
		printf 'install-cli: source launcher not found: %s\n' "$SRC" >&2
		exit 1
	}

	# First writable directory wins.
	for d in $DIRS; do
		if [ -d "$d" ] && [ -w "$d" ]; then
			cp "$SRC" "$d/$CMD"
			chmod +x "$d/$CMD"
			printf 'Installed %s -> %s/%s\n' "$CMD" "$d" "$CMD"
			printf 'Try it:   %s .\n' "$CMD"
			return 0
		fi
	done

	# Nothing writable without elevation: fall back to sudo on /usr/local/bin.
	fallback="/usr/local/bin"
	printf 'No writable install dir found (%s).\n' "$DIRS" >&2
	printf 'Installing to %s with sudo (you may be prompted for your password).\n' "$fallback" >&2
	sudo mkdir -p "$fallback"
	sudo cp "$SRC" "$fallback/$CMD"
	sudo chmod +x "$fallback/$CMD"
	printf 'Installed %s -> %s/%s\n' "$CMD" "$fallback" "$CMD"
	printf 'Try it:   %s .\n' "$CMD"
}

case "${1:-}" in
	--uninstall)
		uninstall
		;;
	-h | --help)
		printf 'Usage: %s [--uninstall]\n' "$0"
		;;
	"")
		install
		;;
	*)
		printf 'install-cli: unknown argument: %s\n' "$1" >&2
		exit 1
		;;
esac

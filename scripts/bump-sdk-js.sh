#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@wvdsh/sdk-js"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FILE="$ROOT/src/dev/sdk-js-version"

usage() {
	cat <<EOF
Usage: ${0##*/} [<version>|--help]

Rewrites src/dev/sdk-js-version, the $PACKAGE version \`wavedash dev\` injects.

  ${0##*/}           pin to the current \`latest\` on npm
  ${0##*/} 1.3.40    pin to an exact version

Keep the pin in step with the \`$PACKAGE\` version play bundles in prod
(play/package.json), and verify a game boots with \`wavedash dev\` after bumping.
EOF
}

case "${1:-}" in
-h | --help)
	usage
	exit 0
	;;
esac

command -v npm >/dev/null 2>&1 || {
	echo "error: npm is required to resolve and verify $PACKAGE versions" >&2
	exit 1
}

[ -f "$FILE" ] || {
	echo "error: $FILE is missing" >&2
	exit 1
}

current="$(tr -d '[:space:]' <"$FILE")"

target="${1:-}"
if [ -z "$target" ]; then
	echo "Resolving latest $PACKAGE from npm..."
	target="$(npm view "$PACKAGE" version)"
else
	case "$target" in
	patch | minor | major)
		echo "error: '$target' is not supported — pass an exact version, or no" >&2
		echo "       argument to pin to the current latest. See --help." >&2
		exit 1
		;;
	esac
	if ! printf '%s' "$target" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$'; then
		echo "error: '$target' is not a semver version" >&2
		exit 1
	fi
	npm view "$PACKAGE@$target" version >/dev/null 2>&1 || {
		echo "error: $PACKAGE@$target is not published on npm" >&2
		exit 1
	}
fi

if [ "$target" = "$current" ]; then
	echo "Already pinned to $target — nothing to do."
	exit 0
fi

printf '%s\n' "$target" >"$FILE"

written="$(tr -d '[:space:]' <"$FILE")"
if [ "$written" != "$target" ]; then
	echo "error: rewrite failed — $FILE now reads '$written'" >&2
	exit 1
fi

echo "Pinned $PACKAGE: $current -> $target"
echo "Next: doppler run -- cargo test, then verify a game boots with \`wavedash dev\`."

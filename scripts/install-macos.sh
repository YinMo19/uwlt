#!/bin/bash
set -euo pipefail
umask 077

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
signing="$HOME/Library/Application Support/uwlt/signing"
app="$HOME/Applications/uwlt.app"
cargo_root=${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}
provision=false
while (($#)); do
    case "$1" in
        --signing-dir|--app|--team)
            [[ $# -ge 2 && -n "$2" ]] || { printf 'Missing value for %s\n' "$1" >&2; exit 2; }
            case "$1" in
                --signing-dir) signing=$2 ;;
                --app) app=$2 ;;
                --team) export UWLT_TEAM_ID=$2 ;;
            esac
            shift 2 ;;
        --provision) provision=true; shift ;;
        -h|--help)
            printf 'Usage: install-macos.sh [--team TEAM_ID] [--provision] [--signing-dir DIRECTORY] [--app PATH.app]\n'
            printf 'Runs cargo install --path . --locked, signs an app bundle, and installs a direct executable symlink.\n'
            printf 'Missing signing configuration is provisioned through Xcode; --provision renews it.\n'
            exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[[ "$app" == /*.app ]] || { printf 'App destination must be an absolute .app path.\n' >&2; exit 2; }
bundle_id=org.uwlt.cli
if [[ -f "$signing/bundle-id" ]]; then IFS= read -r bundle_id < "$signing/bundle-id"; fi
if $provision || [[ ! -f "$signing/uwlt.provisionprofile" ]]; then
    "$root/scripts/provision-macos.sh" --output "$signing" --bundle-id "$bundle_id"
fi
IFS= read -r identity < "$signing/identity"
IFS= read -r bundle_id < "$signing/bundle-id"
binary="$cargo_root/bin/uwlt"
# Refuse unrelated files instead of using cargo --force as an ownership check.
/usr/bin/python3 - "$binary" "$app" "$bundle_id" "$cargo_root" "$root" <<'PY'
import json
import pathlib
import plistlib
import sys

binary, app, bundle_id, cargo_root, root = sys.argv[1:]
binary, app = pathlib.Path(binary), pathlib.Path(app)
if app.is_symlink():
    raise SystemExit("Refusing a symlink app destination: " + str(app))
if app.exists():
    marker = app / "Contents/Resources/uwlt-install-owner"
    info = app / "Contents/Info.plist"
    if not marker.is_file() or marker.read_text() != "uwlt-cli\n":
        raise SystemExit("Refusing to replace an app not installed by uwlt: " + str(app))
    if plistlib.loads(info.read_bytes())["CFBundleIdentifier"] != bundle_id:
        raise SystemExit("Keep the existing bundle ID to retain Keychain access")
if binary.is_symlink():
    if binary.resolve() != (app / "Contents/MacOS/uwlt").resolve():
        raise SystemExit("Refusing an unrelated executable symlink: " + str(binary))
elif binary.exists():
    metadata = pathlib.Path(cargo_root) / ".crates2.json"
    installs = json.loads(metadata.read_text())["installs"] if metadata.exists() else {}
    source = pathlib.Path(root).resolve().as_uri()
    owned = any(
        key.startswith("uwlt ") and key.endswith("(path+" + source + ")")
        and "uwlt" in value["bins"]
        for key, value in installs.items()
    )
    if not owned:
        raise SystemExit("Refusing an executable not installed from this uwlt checkout: " + str(binary))
PY
mkdir -p "$(dirname "$app")" "$cargo_root/bin"
work=$(mktemp -d "$(dirname "$app")/.uwlt-install.XXXXXX")
published=false
committed=false
managed=false
cleanup() {
    status=$?
    if ! $committed; then
        if $managed; then rm -f "$binary"; fi
        if [[ -e "$work/previous-binary" || -L "$work/previous-binary" ]]; then
            mv "$work/previous-binary" "$binary"
        fi
        if $published; then rm -rf "$app"; fi
        if [[ -d "$work/previous.app" ]]; then mv "$work/previous.app" "$app"; fi
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
# Remove the old symlink before cargo runs so it cannot overwrite the signed app.
if [[ -e "$binary" || -L "$binary" ]]; then mv "$binary" "$work/previous-binary"; fi
managed=true
cargo install --path "$root" --locked --force --root "$cargo_root"
"$root/scripts/package-macos.sh" --identity "$identity" \
    --profile "$signing/uwlt.provisionprofile" --bundle-id "$bundle_id" \
    --binary "$binary" --output "$work/uwlt.app"
if [[ -d "$app" ]]; then mv "$app" "$work/previous.app"; fi
mv "$work/uwlt.app" "$app"
published=true
rm "$binary"
ln -s "$app/Contents/MacOS/uwlt" "$binary"
committed=true
printf 'Installed: %s -> %s/Contents/MacOS/uwlt\n' "$binary" "$app"
printf 'Run uwlt from your logged-in macOS session. This local development installation is not notarized.\n'

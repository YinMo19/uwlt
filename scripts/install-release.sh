#!/bin/bash
set -euo pipefail
umask 077

if [[ "${1:-}" == -h || "${1:-}" == --help ]]; then
    printf 'Usage: install.sh [PATH/uwlt.app]\n'
    printf 'Installs the notarized app into ~/Applications and links uwlt in ~/.cargo/bin.\n'
    exit 0
fi
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
source_app=${1:-"$script_dir/uwlt.app"}
app="$HOME/Applications/uwlt.app"
bin_dir="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}/bin"
binary="$bin_dir/uwlt"

bundle_id() {
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$1/Contents/Info.plist"
}
team_id() {
    local details
    details=$(/usr/bin/codesign -dv "$1" 2>&1)
    [[ "$details" =~ TeamIdentifier=([A-Z0-9]+) ]]
    printf '%s\n' "${BASH_REMATCH[1]}"
}

[[ -d "$source_app" && ! -L "$app" ]] || {
    printf 'Expected a uwlt.app source and a non-symlink app destination.\n' >&2
    exit 1
}
if [[ -e "$binary" || -L "$binary" ]]; then
    [[ -L "$binary" && "$(readlink "$binary")" == "$app/Contents/MacOS/uwlt" ]] || {
        printf 'Move the existing file at %s before installing.\n' "$binary" >&2
        exit 1
    }
fi
mkdir -p "$HOME/Applications" "$bin_dir"
work=$(mktemp -d "$HOME/Applications/.uwlt-install.XXXXXX")
published=false
linked=false
committed=false
cleanup() {
    status=$?
    if ! $committed; then
        if $linked; then rm -f "$binary"; fi
        if $published; then rm -rf "$app"; fi
        if [[ -d "$work/previous.app" ]]; then mv "$work/previous.app" "$app"; fi
        if [[ -L "$work/previous-binary" ]]; then mv "$work/previous-binary" "$binary"; fi
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

ditto "$source_app" "$work/uwlt.app"
codesign --verify --strict "$work/uwlt.app"
spctl --assess --type execute "$work/uwlt.app"
[[ "$(bundle_id "$work/uwlt.app")" == org.uwlt.cli ]]
if [[ -d "$app" ]]; then
    IFS= read -r owner < "$app/Contents/Resources/uwlt-install-owner"
    [[ "$owner" == uwlt-cli && "$(bundle_id "$app")" == org.uwlt.cli \
        && "$(team_id "$app")" == "$(team_id "$work/uwlt.app")" ]] || {
        printf 'Existing app has a different owner, bundle ID or signing team.\n' >&2
        exit 1
    }
    mv "$app" "$work/previous.app"
fi
if [[ -L "$binary" ]]; then mv "$binary" "$work/previous-binary"; fi
mv "$work/uwlt.app" "$app"
published=true
ln -s "$app/Contents/MacOS/uwlt" "$binary"
linked=true
committed=true
printf 'Installed: %s -> %s/Contents/MacOS/uwlt\n' "$binary" "$app"
case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) printf 'Add to your shell profile: export PATH="%s:$PATH"\n' "$bin_dir" ;;
esac

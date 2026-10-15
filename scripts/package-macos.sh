#!/bin/bash
set -euo pipefail
umask 077

# TN3137: Data Protection Keychain requires a provisioned app-like bundle.
usage() {
    printf 'Usage: package-macos.sh --identity IDENTITY --profile FILE [--bundle-id org.uwlt.cli]\n'
    printf '                        [--binary target/release/uwlt] [--output dist/uwlt.app]\n'
    printf 'Output must not exist. The profile must authorize the bundle and signing identity.\n'
}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
identity=''
profile=''
bundle_id=org.uwlt.cli
binary="$root/target/release/uwlt"
output="$root/dist/uwlt.app"
while (($#)); do
    case "$1" in
        --identity|--profile|--bundle-id|--binary|--output)
            [[ $# -ge 2 && -n "$2" ]] || { printf 'Missing value for %s\n' "$1" >&2; exit 2; }
            case "$1" in
                --identity) identity=$2 ;;
                --profile) profile=$2 ;;
                --bundle-id) bundle_id=$2 ;;
                --binary) binary=$2 ;;
                --output) output=$2 ;;
            esac
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
[[ -n "$identity" && "$identity" != '-' && -n "$profile" ]] || { usage >&2; exit 2; }
[[ "$output" == *.app && ! -e "$output" && ! -L "$output" ]] || {
    printf 'Output must be a new path ending in .app.\n' >&2
    exit 2
}
parent=$(dirname "$output")
mkdir -p "$parent"
work=$(mktemp -d "$parent/.uwlt-package.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
app="$work/uwlt.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
version=$(cargo pkgid --manifest-path "$root/Cargo.toml" --offline)
version=${version##*#}
version=${version##*@}
security cms -D -i "$profile" -o "$work/profile.plist"
/usr/bin/python3 - "$work/profile.plist" "$bundle_id" "$app" "$work/entitlements.plist" "$version" <<'PY'
import datetime
import pathlib
import plistlib
import re
import sys

profile_file, bundle_id, app_dir, entitlement_file, version = sys.argv[1:]
if not re.fullmatch(r"[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+", bundle_id):
    raise SystemExit("Bundle ID must be a concrete reverse-DNS identifier")
profile = plistlib.loads(pathlib.Path(profile_file).read_bytes())
if profile["ExpirationDate"] <= datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None):
    raise SystemExit("Profile expired; rerun scripts/provision-macos.sh")
if not set(profile["Platform"]).intersection({"OSX", "macOS"}):
    raise SystemExit("Profile does not authorize macOS")
permissions = profile["Entitlements"]
permission = permissions["com.apple.application-identifier"]
app_id = permission.split(".", 1)[0] + "." + bundle_id
if permission != app_id and not (permission.endswith("*") and app_id.startswith(permission[:-1])):
    raise SystemExit("Profile does not authorize this uwlt bundle ID")
groups = permissions.get("keychain-access-groups", [])
if not any(group == app_id or (group.endswith("*") and app_id.startswith(group[:-1])) for group in groups):
    raise SystemExit("Profile does not authorize the app's private keychain access group")
entitlements = {
    "com.apple.application-identifier": app_id,
    "com.apple.developer.team-identifier": permissions["com.apple.developer.team-identifier"],
    "keychain-access-groups": [app_id],
}
# No debug, sandbox, or unrelated capability entitlements.
pathlib.Path(entitlement_file).write_bytes(plistlib.dumps(entitlements))
contents = pathlib.Path(app_dir) / "Contents"
(contents / "Info.plist").write_bytes(plistlib.dumps({
    "CFBundleIdentifier": bundle_id,
    "CFBundleExecutable": "uwlt",
    "CFBundleName": "uwlt",
    "CFBundlePackageType": "APPL",
    "CFBundleVersion": version,
    "CFBundleShortVersionString": version,
    "CFBundleSupportedPlatforms": ["MacOSX"],
    "LSMinimumSystemVersion": "11.0",
    "LSUIElement": True,
}))
(contents / "PkgInfo").write_bytes(b"APPL????")
(contents / "Resources/uwlt-install-owner").write_text("uwlt-cli\n")
PY
install -m 755 "$binary" "$app/Contents/MacOS/uwlt"
install -m 644 "$profile" "$app/Contents/embedded.provisionprofile"
install -m 644 "$root/target/wallet-core-4.8.4/License" "$app/Contents/Resources/WalletCore-License"
install -m 644 "$root/LICENSE" "$app/Contents/Resources/LICENSE"
codesign --force --sign "$identity" --options runtime --timestamp \
    --entitlements "$work/entitlements.plist" "$app"
codesign --verify --strict --verbose=2 "$app"
codesign --display --extract-certificates="$work/signer" "$app"
/usr/bin/python3 - "$work/profile.plist" "$work/signer0" <<'PY'
import pathlib
import plistlib
import sys

profile = plistlib.loads(pathlib.Path(sys.argv[1]).read_bytes())
if pathlib.Path(sys.argv[2]).read_bytes() not in profile["DeveloperCertificates"]:
    raise SystemExit("Signing certificate is not authorized by the provisioning profile")
PY
# Stage on the output filesystem; publish only a signed bundle.
mv -n "$app" "$output"
[[ ! -d "$app" ]] || { printf 'Output appeared while signing; refusing to replace it.\n' >&2; exit 1; }
printf 'Signed bundle: %s\n' "$output"
printf 'Keep the bundle ID and signing team stable to retain access to existing wallets.\n'

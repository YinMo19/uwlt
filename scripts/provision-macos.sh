#!/bin/bash
set -euo pipefail
umask 077

# Xcode owns account authentication; this script never reads account credentials.
team=${UWLT_TEAM_ID:-}
bundle_id=org.uwlt.cli
output="$HOME/Library/Application Support/uwlt/signing"
while (($#)); do
    case "$1" in
        --team|--bundle-id|--output)
            [[ $# -ge 2 && -n "$2" ]] || { printf 'Missing value for %s\n' "$1" >&2; exit 2; }
            case "$1" in
                --team) team=$2 ;;
                --bundle-id) bundle_id=$2 ;;
                --output) output=$2 ;;
            esac
            shift 2 ;;
        -h|--help)
            printf 'Usage: provision-macos.sh --team TEAM_ID [--bundle-id org.uwlt.cli] [--output DIRECTORY]\n'
            printf 'Requires Xcode with a signed-in developer account and xcodegen. Creates a macOS development profile for this Mac.\n'
            exit 0 ;;
        *) printf 'Unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
[[ "$team" =~ ^[A-Z0-9]{10}$ && "$bundle_id" =~ ^[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)+$ ]] || {
    printf 'Supply a ten-character Apple team ID and concrete reverse-DNS bundle ID.\n' >&2
    exit 2
}
xcodegen=$(command -v xcodegen || printf '/opt/homebrew/bin/xcodegen')
work=$(mktemp -d "${TMPDIR:-/tmp}/uwlt-provision.XXXXXX")
trap 'rm -rf "$work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cat > "$work/main.c" <<'C'
int main(void) { return 0; }
C
cat > "$work/uwlt.entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>keychain-access-groups</key><array><string>$(AppIdentifierPrefix)$(PRODUCT_BUNDLE_IDENTIFIER)</string></array>
</dict></plist>
PLIST
cat > "$work/project.yml" <<YAML
name: UwltProvision
options:
  deploymentTarget:
    macOS: "12.0"
targets:
  UwltProvision:
    type: application
    platform: macOS
    attributes:
      SystemCapabilities:
        com.apple.Keychain:
          enabled: 1
    sources:
      - main.c
    settings:
      base:
        PRODUCT_BUNDLE_IDENTIFIER: "$bundle_id"
        DEVELOPMENT_TEAM: "$team"
        CODE_SIGN_STYLE: Automatic
        CODE_SIGN_IDENTITY: Apple Development
        CODE_SIGN_ENTITLEMENTS: uwlt.entitlements
        CODE_SIGN_INJECT_BASE_ENTITLEMENTS: NO
        GENERATE_INFOPLIST_FILE: YES
        ENABLE_APP_SANDBOX: NO
        ENABLE_HARDENED_RUNTIME: YES
YAML
"$xcodegen" generate --spec "$work/project.yml" --project "$work"
if ! xcodebuild -project "$work/UwltProvision.xcodeproj" -scheme UwltProvision \
    -configuration Release -destination 'platform=macOS,arch=arm64' \
    -derivedDataPath "$work/build" -allowProvisioningUpdates \
    -allowProvisioningDeviceRegistration build; then
    printf '\nProvisioning failed; see the Xcode error above. For account errors, open Xcode > Settings > Accounts, sign in to the requested team, complete any 2FA or agreements, then rerun this command.\n' >&2
    exit 1
fi
app="$work/build/Build/Products/Release/UwltProvision.app"
security cms -D -i "$app/Contents/embedded.provisionprofile" -o "$work/profile.plist"
codesign --display --extract-certificates="$work/signer" "$app"
/usr/bin/python3 - "$work/profile.plist" "$work/signer0" "$bundle_id" "$output" "$app" <<'PY'
import hashlib
import pathlib
import plistlib
import shutil
import sys

profile_path, certificate_path, bundle_id, output, app = sys.argv[1:]
profile = plistlib.loads(pathlib.Path(profile_path).read_bytes())
certificate = pathlib.Path(certificate_path).read_bytes()
permission = profile["Entitlements"]["com.apple.application-identifier"]
app_id = permission.split(".", 1)[0] + "." + bundle_id
if permission != app_id and not (permission.endswith("*") and app_id.startswith(permission[:-1])):
    raise SystemExit("Xcode produced a profile that does not authorize this app ID")
if certificate not in profile["DeveloperCertificates"]:
    raise SystemExit("Xcode signer is not authorized by its profile")
destination = pathlib.Path(output)
destination.mkdir(parents=True, exist_ok=True)
shutil.copyfile(pathlib.Path(app) / "Contents/embedded.provisionprofile", destination / "uwlt.provisionprofile")
(destination / "identity").write_text(hashlib.sha1(certificate).hexdigest().upper() + "\n")
(destination / "bundle-id").write_text(bundle_id + "\n")
print("Signing configuration: " + str(destination))
print("Profile expires: " + profile["ExpirationDate"].isoformat())
PY

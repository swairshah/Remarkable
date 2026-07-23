#!/usr/bin/env bash
# Papier iOS OTA release — build a development-signed IPA and publish it
# to the tailnet install page in one shot.
#
#   ./tools/release.sh                # bump patch (1.1.20 -> 1.1.21), build +1
#   VERSION=1.2.0 ./tools/release.sh  # explicit marketing version, build +1
#
# What it does (the whole ritual):
#   1. Bump MARKETING_VERSION / CURRENT_PROJECT_VERSION in project.yml,
#      regenerate the Xcode project (xcodegen).
#   2. xcodebuild archive (automatic signing, team from project.yml).
#   3. xcodebuild -exportArchive with build/export-options.plist
#      (method "debugging" = development-signed).
#   4. Write ota/manifest-<ver>-<build>.plist — the itms-services manifest
#      iOS downloads the IPA through.
#   5. Rewrite ota/index.html — the install page with the tappable
#      "Install X (N)" button.
#   6. scp IPA + manifest + index.html to the VM's ~/papier-install/.
#   7. curl-verify the page, manifest, and IPA over HTTPS.
#
# Requirements: Tailscale up (VM reachable), SSH key auth to the VM,
# Xcode signing working for the team (first run may need Xcode open).

set -euo pipefail
cd "$(dirname "$0")/.."

VM=${VM:-exedev@remarkable.exe.xyz}
INSTALL_DIR=${INSTALL_DIR:-papier-install}
BASE_URL=${BASE_URL:-https://remarkable-vm.tail31aa5e.ts.net/papier-install}
BUNDLE_ID=${BUNDLE_ID:-com.swair.papier}
APP_TITLE=${APP_TITLE:-Papier}

# ---- 1. version bump ------------------------------------------------------
cur_ver=$(sed -n 's/^[[:space:]]*MARKETING_VERSION: "\(.*\)"/\1/p' project.yml)
cur_build=$(sed -n 's/^[[:space:]]*CURRENT_PROJECT_VERSION: "\(.*\)"/\1/p' project.yml)
[[ -n "$cur_ver" && -n "$cur_build" ]] || { echo "cannot read versions from project.yml" >&2; exit 1; }

if [[ -n "${VERSION:-}" ]]; then
  next_ver=$VERSION
else
  IFS=. read -r major minor patch <<<"$cur_ver"
  next_ver="$major.$minor.$((patch + 1))"
fi
next_build=$((cur_build + 1))
tag="$next_ver-$next_build"

echo "==> $cur_ver ($cur_build)  ->  $next_ver ($next_build)"
sed -i '' "s/MARKETING_VERSION: \"$cur_ver\"/MARKETING_VERSION: \"$next_ver\"/; s/CURRENT_PROJECT_VERSION: \"$cur_build\"/CURRENT_PROJECT_VERSION: \"$next_build\"/" project.yml
xcodegen generate >/dev/null

# ---- 2 + 3. archive and export -------------------------------------------
echo "==> archiving"
xcodebuild -project Papier.xcodeproj -scheme Papier \
  -destination 'generic/platform=iOS' -archivePath build/Papier.xcarchive \
  archive -allowProvisioningUpdates -quiet
echo "==> exporting IPA"
rm -rf build/ipa
xcodebuild -exportArchive -archivePath build/Papier.xcarchive \
  -exportOptionsPlist build/export-options.plist -exportPath build/ipa \
  -allowProvisioningUpdates -quiet

# ---- 4. manifest ----------------------------------------------------------
cat > "ota/manifest-$tag.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>items</key><array><dict>
  <key>assets</key><array><dict>
    <key>kind</key><string>software-package</string>
    <key>url</key><string>$BASE_URL/$APP_TITLE-$tag.ipa</string>
  </dict></array>
  <key>metadata</key><dict>
    <key>bundle-identifier</key><string>$BUNDLE_ID</string>
    <key>bundle-version</key><string>$next_ver</string>
    <key>kind</key><string>software</string>
    <key>title</key><string>$APP_TITLE</string>
  </dict>
</dict></array></dict></plist>
EOF

# ---- 5. install page ------------------------------------------------------
cat > ota/index.html <<EOF
<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1">
<body style="font-family:-apple-system;display:grid;place-items:center;min-height:90vh;background:#fbfbf9">
<div style="text-align:center">
<h1 style="font-weight:600">$APP_TITLE</h1>
<a style="font-size:22px;padding:14px 28px;background:#1c1c1c;color:#fff;border-radius:12px;text-decoration:none"
   href="itms-services://?action=download-manifest&amp;url=$BASE_URL/manifest-$tag.plist">Install $next_ver ($next_build)</a>
<p style="color:#888;margin-top:24px">Current package: $next_ver ($next_build)<br>After install: Settings &gt; General &gt; VPN &amp; Device Management<br>&gt; trust the developer certificate if asked.</p>
</div></body>
EOF

# ---- 6. upload ------------------------------------------------------------
echo "==> uploading to $VM:~/$INSTALL_DIR/"
cp build/ipa/Papier.ipa "/tmp/$APP_TITLE-$tag.ipa"
scp -o BatchMode=yes "/tmp/$APP_TITLE-$tag.ipa" "ota/manifest-$tag.plist" ota/index.html \
  "$VM:~/$INSTALL_DIR/"

# ---- 7. verify ------------------------------------------------------------
echo "==> verifying"
curl -fsS -m 10 "$BASE_URL/" | grep -o "Install [0-9.]* ([0-9]*)"
curl -fsS -m 10 -o /dev/null -w "manifest: %{http_code}\n" "$BASE_URL/manifest-$tag.plist"
curl -fsS -m 30 -o /dev/null -w "ipa: %{http_code} (%{size_download} bytes)\n" "$BASE_URL/$APP_TITLE-$tag.ipa"

echo
echo "Install page: $BASE_URL/"
echo "Remember to commit: project.yml (auto-saved), ota/manifest-$tag.plist, ota/index.html, build/"

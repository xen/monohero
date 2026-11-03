#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in Darwin) ;; *) echo 'macOS is required' >&2; exit 1;; esac
export MACOSX_DEPLOYMENT_TARGET=15.0
cargo +1.99.1 build --release --locked --manifest-path desktop/Cargo.toml
bundle=dist/Monohero.app
mkdir -p "$bundle/Contents/MacOS"
cp desktop/target/release/monohero-ui "$bundle/Contents/MacOS/monohero-ui"
cat > "$bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.xen.monohero</string>
<key>CFBundleName</key><string>Monohero</string>
<key>CFBundleDisplayName</key><string>Monohero</string>
<key>CFBundleExecutable</key><string>monohero-ui</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.4.0</string>
<key>CFBundleVersion</key><string>4</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>LSUIElement</key><true/>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$bundle"
printf 'Built %s\n' "$bundle"

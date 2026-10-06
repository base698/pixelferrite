#!/bin/sh
# Builds dist/Pixelferrite.app for this Mac and puts a shortcut on the Desktop.
set -eu
cd "$(dirname "$0")/.."

cargo build --release -p pixelferrite

app=dist/Pixelferrite.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/pixelferrite/Cargo.toml | head -1)
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/pixelferrite "$app/Contents/MacOS/pixelferrite"

set=$(mktemp -d)/Pixelferrite.iconset
mkdir "$set"
for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon.png --out "$set/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon.png --out "$set/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set" -o "$app/Contents/Resources/Pixelferrite.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Pixelferrite</string>
    <key>CFBundleDisplayName</key><string>Pixelferrite</string>
    <key>CFBundleIdentifier</key><string>org.pixelferrite.Pixelferrite</string>
    <key>CFBundleExecutable</key><string>pixelferrite</string>
    <key>CFBundleIconFile</key><string>Pixelferrite</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.graphics-design</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$app"

if [ "${1:-}" != "--no-shortcut" ]; then
    # A Finder alias rather than a symlink: Finder draws a symlink to an app
    # with a blank icon. An existing alias keeps working across rebuilds.
    if [ ! -e "$HOME/Desktop/Pixelferrite" ]; then
        osascript -e "tell application \"Finder\" to make alias file to (POSIX file \"$PWD/$app\") at desktop" \
            -e 'tell application "Finder" to set name of result to "Pixelferrite"' >/dev/null
    fi
    echo "shortcut: $HOME/Desktop/Pixelferrite"
fi
echo "built: $PWD/$app"

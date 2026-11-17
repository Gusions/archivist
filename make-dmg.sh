#!/bin/bash
# Crea dist/Archivist.app e dist/Archivist.dmg (da eseguire su macOS)
set -euo pipefail
cd "$(dirname "$0")"
command -v cargo >/dev/null || { echo "Installa Rust: https://rustup.rs"; exit 1; }
cargo build --release --features window

APP="dist/Archivist.app"
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/archivist "$APP/Contents/MacOS/archivist-bin"
cp assets/Archivist.icns "$APP/Contents/Resources/Archivist.icns"

# lanciatore: apre la finestra nativa
cat > "$APP/Contents/MacOS/Archivist" <<'SH'
#!/bin/bash
exec "$(dirname "$0")/archivist-bin" window
SH
chmod +x "$APP/Contents/MacOS/Archivist"

cat > "$APP/Contents/Info.plist" <<'PL'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Archivist</string>
  <key>CFBundleDisplayName</key><string>Archivist</string>
  <key>CFBundleIdentifier</key><string>it.archivist.app</string>
  <key>CFBundleIconFile</key><string>Archivist</string>
  <key>CFBundleExecutable</key><string>Archivist</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PL

# firma ad-hoc (senza account sviluppatore): evita l'errore "danneggiata"
codesign --force --deep --sign - "$APP" 2>/dev/null || true

# dmg con collegamento ad Applicazioni
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Archivist" -srcfolder "$STAGE" -ov -format UDZO dist/Archivist.dmg
rm -rf "$STAGE"
echo "Fatto: dist/Archivist.dmg"

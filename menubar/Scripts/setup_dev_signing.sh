#!/usr/bin/env bash
# Setup stable development code signing to stop Keychain prompts on every rebuild.
#
# Ad-hoc signing produces a fresh signature each build, so macOS treats every
# rebuild as a new app and re-prompts for Keychain access. This script creates
# a self-signed cert that stays consistent across rebuilds, so "Always Allow"
# sticks.
set -euo pipefail

CERT_NAME="MenuBar Dev"

echo "🔐 Setting up stable development code signing for $CERT_NAME..."
echo ""

# Bail early if the cert already exists.
if security find-certificate -c "$CERT_NAME" >/dev/null 2>&1; then
    echo "✅ Certificate '$CERT_NAME' already exists in your keychain."
    echo ""
    echo "If you haven't already, add this to your shell profile (~/.zshrc):"
    echo ""
    echo "    export APP_IDENTITY='$CERT_NAME'"
    echo ""
    echo "Then restart your terminal and rebuild:"
    echo ""
    echo "    ./Scripts/package.sh && open MenuBar.app"
    exit 0
fi

echo "Creating self-signed certificate '$CERT_NAME'..."
echo ""

TEMP_CONFIG=$(mktemp)
trap 'rm -f "$TEMP_CONFIG"' EXIT

cat > "$TEMP_CONFIG" <<EOF
[ req ]
distinguished_name = req_distinguished_name
x509_extensions = v3_req
prompt = no

[ req_distinguished_name ]
CN = $CERT_NAME
O = MenuBar Development
C = US

[ v3_req ]
keyUsage = critical,digitalSignature
extendedKeyUsage = codeSigning
EOF

# Generate cert + private key.
openssl req -x509 -newkey rsa:4096 -sha256 -days 3650 \
    -nodes -keyout /tmp/menubar-dev.key -out /tmp/menubar-dev.crt \
    -config "$TEMP_CONFIG" 2>/dev/null

# Pack into PKCS12 so Keychain can import it.
openssl pkcs12 -export -out /tmp/menubar-dev.p12 \
    -inkey /tmp/menubar-dev.key -in /tmp/menubar-dev.crt \
    -passout pass: 2>/dev/null

# Import into login keychain, granting codesign + security access.
security import /tmp/menubar-dev.p12 \
    -k ~/Library/Keychains/login.keychain-db \
    -T /usr/bin/codesign -T /usr/bin/security

# Wipe the temp key material — Keychain has its own copy now.
rm -f /tmp/menubar-dev.{key,crt,p12}

echo ""
echo "✅ Certificate created and imported."
echo ""
echo "⚠️  ONE-TIME MANUAL STEP — trust the cert for code signing:"
echo ""
echo "  1. Open Keychain Access.app"
echo "  2. In the 'login' keychain, find '$CERT_NAME'"
echo "  3. Double-click it"
echo "  4. Expand 'Trust'"
echo "  5. Set 'Code Signing' → 'Always Trust'"
echo "  6. Close the dialog (enter your password when prompted)"
echo ""
echo "Then add this to your shell profile (~/.zshrc or ~/.bashrc):"
echo ""
echo "    export APP_IDENTITY='$CERT_NAME'"
echo ""
echo "Restart your terminal and rebuild:"
echo ""
echo "    ./Scripts/package.sh && open MenuBar.app"
echo ""
echo "First launch will still prompt once for Keychain access — click"
echo "\"Always Allow\". After that, no more prompts across rebuilds."

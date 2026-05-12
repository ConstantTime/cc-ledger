#!/usr/bin/env bash
# cc-ledger installer.
#
#   curl -fsSL https://ccledger.dev/install | bash
#
# Env overrides:
#   CC_LEDGER_VERSION       pin a version (default: contents of /latest)
#   CC_LEDGER_INSTALL_DIR   install location (default: $HOME/.local/bin)
#   CC_LEDGER_NO_HOOKS=1    skip the trailing `cc-ledger install`

set -euo pipefail

BLOB_BASE="https://taea7hakf9g56hd8.public.blob.vercel-storage.com"

INSTALL_DIR="${CC_LEDGER_INSTALL_DIR:-$HOME/.local/bin}"

c_bold=$'\033[1m'
c_dim=$'\033[2m'
c_red=$'\033[31m'
c_green=$'\033[32m'
c_reset=$'\033[0m'

info() { printf '%s==>%s %s\n' "$c_bold" "$c_reset" "$*"; }
warn() { printf '%swarn:%s %s\n' "$c_red" "$c_reset" "$*" >&2; }
die()  { printf '%serror:%s %s\n' "$c_red" "$c_reset" "$*" >&2; exit 1; }

if [[ "$(id -u)" -eq 0 ]]; then
  die "refuse to run as root — installs to $INSTALL_DIR which doesn't need sudo"
fi

uname_s=$(uname -s)
uname_m=$(uname -m)

case "$uname_s" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) die "unsupported OS: $uname_s (cc-ledger ships macOS + Linux only)" ;;
esac

case "$uname_m" in
  x86_64|amd64)        arch=x86_64 ;;
  arm64|aarch64)       arch=aarch64 ;;
  *) die "unsupported arch: $uname_m" ;;
esac

case "$os-$arch" in
  darwin-x86_64)  target=x86_64-apple-darwin ;;
  darwin-aarch64) target=aarch64-apple-darwin ;;
  linux-x86_64)   target=x86_64-unknown-linux-musl ;;
  linux-aarch64)  target=aarch64-unknown-linux-musl ;;
esac

if [[ -n "${CC_LEDGER_VERSION:-}" ]]; then
  version="$CC_LEDGER_VERSION"
  info "pinned to version $version"
else
  info "resolving latest version"
  if ! latest_raw=$(curl -fsSL "$BLOB_BASE/latest" 2>&1); then
    die "could not fetch $BLOB_BASE/latest — has any release been published yet? ($latest_raw)"
  fi
  version=$(printf '%s' "$latest_raw" | tr -d '[:space:]')
  [[ -n "$version" ]] || die "got empty version string from $BLOB_BASE/latest"
fi

tarball="cc-ledger-${target}.tar.gz"
url="$BLOB_BASE/versions/${version}/${tarball}"
sums_url="$BLOB_BASE/versions/${version}/SHA256SUMS"

info "platform   $os/$arch ($target)"
info "version    $version"
info "source     $url"

tmpdir=$(mktemp -d -t cc-ledger.XXXXXX)
trap 'rm -rf "$tmpdir"' EXIT

info "downloading"
curl -fSL "$url" -o "$tmpdir/$tarball"
curl -fSL "$sums_url" -o "$tmpdir/SHA256SUMS"

info "verifying checksum"
(
  cd "$tmpdir"
  # Pull just the line for our tarball — works on macOS (no --ignore-missing).
  if ! grep " $tarball\$" SHA256SUMS > expected.sums; then
    die "no checksum entry for $tarball in SHA256SUMS"
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c expected.sums >/dev/null
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c expected.sums >/dev/null
  else
    die "neither sha256sum nor shasum available — can't verify download"
  fi
)

info "extracting to $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
tar -xzf "$tmpdir/$tarball" -C "$tmpdir"
[[ -f "$tmpdir/cc-ledger" ]] || die "tarball did not contain cc-ledger binary"
mv "$tmpdir/cc-ledger" "$INSTALL_DIR/cc-ledger"
chmod +x "$INSTALL_DIR/cc-ledger"

printf '%s✓%s installed %s/cc-ledger\n' "$c_green" "$c_reset" "$INSTALL_DIR"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    rc_hint="$HOME/.zshrc"
    [[ "${SHELL:-}" == */bash ]] && rc_hint="$HOME/.bashrc"
    cat <<EOF

${c_dim}note:${c_reset} $INSTALL_DIR is not on your \$PATH. Add it with:

    echo 'export PATH="$INSTALL_DIR:\$PATH"' >> $rc_hint
    source $rc_hint

EOF
    ;;
esac

if [[ "${CC_LEDGER_NO_HOOKS:-}" == "1" ]]; then
  info "skipping cc-ledger install (CC_LEDGER_NO_HOOKS=1)"
else
  info "wiring Claude Code hooks"
  "$INSTALL_DIR/cc-ledger" install
fi

printf '\n%s✓ done.%s try %s`cc-ledger stats`%s after a Claude Code session.\n' \
  "$c_green" "$c_reset" "$c_bold" "$c_reset"

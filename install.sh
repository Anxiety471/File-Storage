#!/bin/sh
# Install Drawer: curl -fsSL https://raw.githubusercontent.com/Anxiety471/File-Storage/main/install.sh | sh
#
# Downloads the prebuilt release for this machine and verifies its checksum.
# If there is no release for this platform yet, builds from source with cargo.
#
# Environment overrides:
#   DRAWER_PREFIX   install root, default ~/.local (binary in bin/, UI in share/drawer/ui)
#   DRAWER_VERSION  release tag to install, default latest
#   DRAWER_REF      branch used for the source fallback, default main
#   DRAWER_REPO     GitHub owner/repo, default Anxiety471/File-Storage
#   DRAWER_SOURCE=1 skip the release download and build from source
set -eu

REPO="${DRAWER_REPO:-Anxiety471/File-Storage}"
VERSION="${DRAWER_VERSION:-latest}"
REF="${DRAWER_REF:-main}"
PREFIX="${DRAWER_PREFIX:-$HOME/.local}"

say() { printf '==> %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  case "$os-$arch" in
    Linux-x86_64) echo x86_64-unknown-linux-gnu ;;
    Linux-aarch64 | Linux-arm64) echo aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) echo x86_64-apple-darwin ;;
    Darwin-arm64) echo aarch64-apple-darwin ;;
    *) echo "" ;;
  esac
}

sha256_check() {
  # $1 = checksum file line(s) for the archive, run from the archive's directory
  if have sha256sum; then
    sha256sum -c "$1" >/dev/null
  elif have shasum; then
    shasum -a 256 -c "$1" >/dev/null
  else
    say "no sha256sum or shasum found, skipping checksum"
    return 0
  fi
}

install_tree() {
  # $1 = binary path, $2 = UI dist dir or empty
  mkdir -p "$PREFIX/bin"
  cp "$1" "$PREFIX/bin/drawer"
  chmod 755 "$PREFIX/bin/drawer"
  if [ -n "$2" ] && [ -f "$2/index.html" ]; then
    mkdir -p "$PREFIX/share/drawer"
    rm -rf "$PREFIX/share/drawer/ui"
    cp -R "$2" "$PREFIX/share/drawer/ui"
  fi
}

install_release() {
  target=$1
  if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
  else
    base="https://github.com/$REPO/releases/download/$VERSION"
  fi
  archive="drawer-$target.tar.gz"
  say "downloading $archive ($VERSION)"
  curl --proto '=https' --tlsv1.2 -fsSL "$base/$archive" -o "$tmp/$archive" || return 1
  curl --proto '=https' --tlsv1.2 -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || return 1
  grep " $archive\$" "$tmp/SHA256SUMS" > "$tmp/check" || die "$archive is not listed in SHA256SUMS"
  (cd "$tmp" && sha256_check check) || die "checksum mismatch for $archive"
  tar -xzf "$tmp/$archive" -C "$tmp"
  install_tree "$tmp/drawer-$target/drawer" "$tmp/drawer-$target/ui"
}

install_source() {
  if ! have cargo; then
    say "cargo not found, installing Rust with rustup (minimal profile)"
    have curl || die "curl is required"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
  fi
  say "downloading source ($REF)"
  curl --proto '=https' --tlsv1.2 -fsSL "https://codeload.github.com/$REPO/tar.gz/refs/heads/$REF" -o "$tmp/src.tar.gz"
  mkdir "$tmp/src"
  tar -xzf "$tmp/src.tar.gz" -C "$tmp/src" --strip-components=1
  ui_dist=""
  if have npm; then
    say "building the UI"
    (cd "$tmp/src/ui" && npm ci --no-audit --no-fund && npm run build)
    ui_dist="$tmp/src/ui/dist"
  else
    say "npm not found, skipping the UI (the API and MCP server still work)"
  fi
  say "building drawer with cargo (this takes a few minutes)"
  (cd "$tmp/src" && cargo build --release)
  install_tree "$tmp/src/target/release/drawer" "$ui_dist"
}

main() {
  have curl || die "curl is required"
  have tar || die "tar is required"
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/drawer-install.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT INT TERM

  target=$(detect_target)
  installed=""
  if [ -n "$target" ] && [ "${DRAWER_SOURCE:-0}" != 1 ]; then
    if install_release "$target"; then
      installed=release
    else
      say "no release for $target yet, falling back to a source build"
    fi
  fi
  if [ -z "$installed" ]; then
    install_source
  fi

  say "installed drawer to $PREFIX/bin/drawer"
  case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) say "add $PREFIX/bin to your PATH, e.g.: export PATH=\"$PREFIX/bin:\$PATH\"" ;;
  esac
  say "start it with: drawer serve   (then open http://127.0.0.1:8787)"
  say "data goes to ./data unless you pass --data-dir or set DRAWER_DATA_DIR"
}

main "$@"

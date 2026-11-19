#!/bin/sh
# multillm CLI installer (Linux / macOS)
#   curl -fsSL https://raw.githubusercontent.com/RandomPixelStudios/Multi-LLM/HEAD/install.sh | sh
#
# Env: MULTILLM_INSTALL_DIR (default ~/.local/bin, /usr/local/bin as root)
#      MULTILLM_VERSION     (release tag, default: latest)
set -eu

REPO="RandomPixelStudios/Multi-LLM"
say() { printf '\033[38;2;124;140;255m●\033[0m %s\n' "$*"; }
die() { printf '\033[31m✗\033[0m %s\n' "$*" >&2; exit 1; }

os=$(uname -s); arch=$(uname -m)
case "$os" in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) die "Nicht unterstütztes System: $os (Windows: install.ps1 verwenden)" ;;
esac
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) die "Nicht unterstützte Architektur: $arch" ;;
esac

if [ -n "${MULTILLM_INSTALL_DIR:-}" ]; then dir="$MULTILLM_INSTALL_DIR"
elif [ "$(id -u)" = "0" ]; then dir=/usr/local/bin
else dir="$HOME/.local/bin"; fi
mkdir -p "$dir"

asset="multillm-$os-$arch.tar.gz"
if [ -n "${MULTILLM_VERSION:-}" ]; then base="https://github.com/$REPO/releases/download/$MULTILLM_VERSION"
else base="https://github.com/$REPO/releases/latest/download"; fi

dl() { if command -v curl >/dev/null 2>&1; then curl -fsSL "$1" -o "$2"; else wget -qO "$2" "$1"; fi; }
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

say "Lade $asset …"
if dl "$base/$asset" "$tmp/$asset"; then
  if dl "$base/$asset.sha256" "$tmp/sum" 2>/dev/null; then
    want=$(cut -d' ' -f1 < "$tmp/sum")
    if command -v sha256sum >/dev/null 2>&1; then got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
    else got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1); fi
    [ "$want" = "$got" ] || die "Prüfsumme stimmt nicht – Download abgebrochen."
  fi
  tar xzf "$tmp/$asset" -C "$tmp"
  install -m 755 "$tmp/multillm" "$dir/multillm" 2>/dev/null || { cp "$tmp/multillm" "$dir/multillm"; chmod 755 "$dir/multillm"; }
else
  say "Kein fertiges Binary gefunden – baue aus dem Quellcode (benötigt Rust/cargo) …"
  command -v cargo >/dev/null 2>&1 || die "cargo nicht gefunden. Rust installieren: https://rustup.rs"
  cargo install --locked --git "https://github.com/$REPO" multillm --root "$tmp/root"
  install -m 755 "$tmp/root/bin/multillm" "$dir/multillm"
fi

say "Installiert: $dir/multillm"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) printf '  %s nicht im PATH. Hinzufügen mit:\n    echo '\''export PATH="%s:$PATH"'\'' >> ~/.profile\n' "$dir" "$dir" ;;
esac
say "Starten mit:  multillm"

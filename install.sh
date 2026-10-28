#!/bin/sh
# Installs omoi on Linux or macOS and sets up Omoikane in Claude Code.
#   curl -fsSL https://raw.githubusercontent.com/marcmayol/omoikane/master/install.sh | sh
#
# Optional variables:
#   OMOIKANE_INSTALL_DIR   where the binary goes (default ~/.local/bin)
#   OMOIKANE_VERSION       a specific version, e.g. v0.1.0 (default: the latest)
#   OMOIKANE_NO_MODEL=1    don't download the model (~150 MB; without it, search by words only)
#   OMOIKANE_NO_SETUP=1    don't touch Claude Code
set -eu

repo="marcmayol/omoikane"
dir="${OMOIKANE_INSTALL_DIR:-$HOME/.local/bin}"
if [ -n "${OMOIKANE_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/$OMOIKANE_VERSION"
else
  base="https://github.com/$repo/releases/latest/download"
fi

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  *)
    echo "omoikane: no prebuilt binary for $(uname -s) $(uname -m); build it with: cargo install --git https://github.com/$repo" >&2
    exit 1
    ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
paquete="omoi-$target.tar.gz"
echo "Downloading $paquete..."
curl -fsSL "$base/$paquete" -o "$tmp/$paquete"
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"

# The checksum must match before anything is installed.
esperada="$(grep " $paquete\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)"
if command -v sha256sum >/dev/null 2>&1; then
  real="$(sha256sum "$tmp/$paquete" | cut -d' ' -f1)"
else
  real="$(shasum -a 256 "$tmp/$paquete" | cut -d' ' -f1)"
fi
if [ -z "$esperada" ] || [ "$esperada" != "$real" ]; then
  echo "omoikane: the SHA-256 checksum of $paquete doesn't match; nothing was installed." >&2
  exit 1
fi

tar -xzf "$tmp/$paquete" -C "$tmp"
mkdir -p "$dir"
mv "$tmp/omoi" "$dir/omoi"
chmod +x "$dir/omoi"
echo "Installed: $dir/omoi ($("$dir/omoi" --version))"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "Note: $dir is not on your PATH. Add it (for example in ~/.profile): export PATH=\"$dir:\$PATH\"" ;;
esac

if [ -z "${OMOIKANE_NO_MODEL:-}" ]; then
  "$dir/omoi" model install || echo "omoikane: couldn't install the model; try again with: omoi model install" >&2
fi
if [ -z "${OMOIKANE_NO_SETUP:-}" ]; then
  "$dir/omoi" setup claude --yes || echo "omoikane: Claude Code isn't set up yet; once you have it, run: omoi setup claude" >&2
fi

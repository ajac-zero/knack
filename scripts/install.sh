#!/bin/sh
# Install a prebuilt knack CLI binary from GitHub Releases.
set -eu

repo='ajac-zero/knack'
package='knack'
version="${KNACK_VERSION:-}"
install_dir="${KNACK_INSTALL_DIR:-$HOME/.local/bin}"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    printf 'knack installer requires %s\n' "$1" >&2
    exit 1
  }
}

need curl
need tar

os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
  Linux/x86_64) target='x86_64-unknown-linux-gnu' ;;
  Linux/aarch64|Linux/arm64) target='aarch64-unknown-linux-gnu' ;;
  Darwin/x86_64) target='x86_64-apple-darwin' ;;
  Darwin/arm64) target='aarch64-apple-darwin' ;;
  *)
    printf 'Unsupported platform: %s/%s\n' "$os" "$arch" >&2
    exit 1
    ;;
esac

if [ -z "$version" ]; then
  version="$(curl --fail --silent --show-error "https://api.github.com/repos/$repo/tags?per_page=100" \
    | grep -o '"name": "knack-v[^"]*"' \
    | head -n 1 \
    | cut -d '"' -f 4 \
    | sed 's/^knack-v//')"
fi

if [ -z "$version" ]; then
  printf 'Could not determine the latest knack release. Set KNACK_VERSION explicitly.\n' >&2
  exit 1
fi

archive="$package-$version-$target.tar.gz"
url="https://github.com/$repo/releases/download/$package-v$version/$archive"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT INT HUP TERM

curl --fail --location --silent --show-error --output "$tmpdir/$archive" "$url"
curl --fail --location --silent --show-error --output "$tmpdir/$archive.sha256" "$url.sha256"

if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmpdir" && sha256sum -c "$archive.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmpdir" && shasum -a 256 -c "$archive.sha256")
else
  printf 'Install sha256sum or shasum to verify the download.\n' >&2
  exit 1
fi

mkdir -p "$install_dir"
tar -xzf "$tmpdir/$archive" -C "$install_dir"
printf 'Installed %s %s to %s/%s\n' "$package" "$version" "$install_dir" "$package"
case ":$PATH:" in
  *":$install_dir:"*) ;;
  *) printf 'Add %s to your PATH to use knack.\n' "$install_dir" ;;
esac

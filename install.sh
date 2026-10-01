#!/bin/sh
# Installs the `calyx` binary from the GitHub releases (decision D35).
#
#   curl -fsSL https://raw.githubusercontent.com/daltonfontes/calyx/main/install.sh | sh
#
# Nothing else is needed: the binary is self-contained (no Rust, no C
# compiler, no libraries). Variables:
#   CALYX_VERSION   a release tag, e.g. v0.1.0 (default: the latest)
#   CALYX_INSTALL   where to install (default: ~/.calyx); the binary goes
#                   in $CALYX_INSTALL/bin
#   CALYX_BASE_URL  where releases are downloaded from (for mirrors and tests)

set -eu

repo="daltonfontes/calyx"
version="${CALYX_VERSION:-latest}"
install_dir="${CALYX_INSTALL:-$HOME/.calyx}"

fail() {
    echo "calyx install: $*" >&2
    exit 1
}

case "$(uname -s)" in
    Linux) os="unknown-linux-musl" ;;
    Darwin) os="apple-darwin" ;;
    *) fail "no prebuilt binary for $(uname -s); on Windows, use WSL" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) fail "no prebuilt binary for $(uname -m)" ;;
esac
target="$arch-$os"

if [ -n "${CALYX_BASE_URL:-}" ]; then
    base="$CALYX_BASE_URL"
elif [ "$version" = "latest" ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi
archive="calyx-$target.tar.gz"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -q "$1" -O "$2"; }
else
    fail "needs curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    fail "needs sha256sum or shasum to check the download"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "downloading $archive ($version)"
fetch "$base/$archive" "$tmp/$archive" || fail "cannot download $base/$archive"
fetch "$base/$archive.sha256" "$tmp/$archive.sha256" || fail "cannot download the checksum"

expected="$(cut -d' ' -f1 <"$tmp/$archive.sha256")"
actual="$(sha256 "$tmp/$archive")"
[ "$expected" = "$actual" ] || fail "checksum mismatch for $archive (expected $expected, got $actual)"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$install_dir/bin"
# Move into place, so a running calyx is never overwritten half-way.
cp "$tmp/calyx" "$install_dir/bin/calyx.new"
chmod 755 "$install_dir/bin/calyx.new"
mv "$install_dir/bin/calyx.new" "$install_dir/bin/calyx"

echo "installed $("$install_dir/bin/calyx" --version) in $install_dir/bin/calyx"
case ":$PATH:" in
    *":$install_dir/bin:"*) ;;
    *)
        echo
        echo "add it to your PATH, e.g. in ~/.bashrc or ~/.zshrc:"
        echo "    export PATH=\"$install_dir/bin:\$PATH\""
        ;;
esac

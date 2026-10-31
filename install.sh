#!/bin/sh
# Install the latest native Carry release without a Rust toolchain.
set -eu

fail() {
    printf 'carry install: %s\n' "$*" >&2
    exit 1
}

system=$(uname -s)
architecture=$(uname -m)
case "$system/$architecture" in
    Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
    Darwin/arm64) target=aarch64-apple-darwin ;;
    Darwin/x86_64) target=x86_64-apple-darwin ;;
    *) fail "No release binary for $system/$architecture. Build from source instead." ;;
esac

command -v curl >/dev/null 2>&1 || fail 'curl is required.'
if command -v sha256sum >/dev/null 2>&1; then
    checksum=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    checksum=shasum
else
    fail 'sha256sum or shasum is required.'
fi

repository=https://github.com/wiggzz/carry
release=$(curl -fsSL -o /dev/null -w '%{url_effective}' "$repository/releases/latest")
version=${release##*/}
case "$version" in
    ''|*[!A-Za-z0-9._-]*) fail 'Could not determine the latest release.' ;;
esac
case "$release" in
    "$repository/releases/tag/"*) ;;
    *) fail 'Could not determine the latest release.' ;;
esac
archive="carry-$version-$target.tar.gz"
url="$repository/releases/download/$version"
install_dir=${CARRY_INSTALL_DIR:-"$HOME/.local/bin"}
temporary=$(mktemp -d "${TMPDIR:-/tmp}/carry-install.XXXXXX")
trap 'rm -rf "$temporary"' 0
trap 'exit 1' 1 2 3 15

printf 'Downloading Carry %s for %s...\n' "$version" "$target"
curl -fsSL -o "$temporary/$archive" "$url/$archive"
curl -fsSL -o "$temporary/SHA256SUMS" "$url/SHA256SUMS"
(
    cd "$temporary"
    awk -v archive="$archive" '$2 == archive { print; count++ } END { if (count != 1) exit 1 }' \
        SHA256SUMS > selected-checksum || fail 'Release checksum is missing or ambiguous.'
    if [ "$checksum" = sha256sum ]; then
        sha256sum -c selected-checksum
    else
        shasum -a 256 -c selected-checksum
    fi
    tar -xzf "$archive" carry
)
mkdir -p "$install_dir"
install -m 0755 "$temporary/carry" "$install_dir/carry"
printf 'Installed Carry to %s/carry\n' "$install_dir"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf 'Add %s to PATH in your shell configuration, then run carry --help.\n' "$install_dir" ;;
esac

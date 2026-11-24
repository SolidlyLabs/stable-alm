#!/bin/bash
# Sign and verify the combined Windows and Linux release from Linux.
set -euo pipefail

usage() {
    printf 'Usage: %s <signer> <key-path | --verify>\n' "$0" >&2
    exit 2
}
[[ $# == 2 ]] || usage
signer=$1
key=$2
if [[ $key != --verify ]]; then key=$(realpath -e -- "$key"); fi
release=$(cd -- "$(dirname -- "$0")" && pwd)
version=$(sed -n 's/^version = "\([0-9]*\.[0-9]*\.[0-9]*\)"$/\1/p' "$release/../Cargo.toml")
[[ -n $version ]] || { printf 'Missing release version\n' >&2; exit 1; }
assets=("StableALM-Setup-$version.exe" StableALM.exe "StableALM-$version-linux-x86_64.tar.gz")
sums=SHA256SUMS.txt

cd -- "$release/../dist/$version"
verify() {
    ssh-keygen -Y verify -f "$release/allowed_signers" -I "$signer" -n stable-alm-release -s "$1.sig" < "$1"
    if ! sha256sum -- "${assets[@]}" | cmp -s - "$1"; then
        printf 'The signed manifest must match all three release assets\n' >&2
        return 1
    fi
    sha256sum --check --strict -- "$1"
}

if [[ $key == --verify ]]; then
    verify "$sums"
else
    stage=$(mktemp -d .sign.XXXXXX)
    trap 'for file in "$stage/$sums" "$stage/$sums.sig"; do if [[ -e $file ]]; then unlink "$file"; fi; done; rmdir "$stage"' EXIT
    sha256sum -- "${assets[@]}" > "$stage/$sums"
    ssh-keygen -Y sign -f "$key" -n stable-alm-release "$stage/$sums"
    verify "$stage/$sums"
    mv -f -- "$stage/$sums" "$stage/$sums.sig" .
fi

#!/bin/sh
# Builds the release binary and reports its size and the heaviest crates.
#
#   scripts/size-report.sh            # size + top-10 crates (needs cargo-bloat)
#   WITH_UPX=1 scripts/size-report.sh # also compare with an UPX-packed copy
#   FEATURES="mpris" scripts/size-report.sh
set -eu
cd "$(dirname "$0")/.."

feat=${FEATURES:+--features "$FEATURES"}
flags=$(scripts/rustflags.sh)

RUSTFLAGS="$flags" cargo build --release --locked $feat
bin=target/release/haste
size=$(stat -c %s "$bin")
echo
echo "binary: $bin"
printf 'size:   %d bytes (%s KiB)\n' "$size" "$((size / 1024))"
echo "dynamic deps:"
readelf -d "$bin" | sed -n 's/.*NEEDED.*\[\(.*\)\]/  \1/p'

if command -v cargo-bloat >/dev/null 2>&1; then
    echo
    echo "top-10 crates (.text, unstripped analysis build):"
    # cargo-bloat needs symbols; analyse an otherwise identical unstripped build.
    RUSTFLAGS="$flags" CARGO_PROFILE_RELEASE_STRIP=false \
        cargo bloat --release --locked $feat --crates -n 10 2>/dev/null | sed -n '/File  .text/,/section size/p'
else
    echo "(install cargo-bloat for a per-crate breakdown: cargo install cargo-bloat)"
fi

if [ "${WITH_UPX:-0}" = 1 ]; then
    if command -v upx >/dev/null 2>&1; then
        cp "$bin" target/haste.upx
        upx -q --best --lzma target/haste.upx >/dev/null
        printf '\nUPX --best --lzma: %d bytes (not used for packages)\n' "$(stat -c %s target/haste.upx)"
    else
        echo "upx not found"
    fi
fi

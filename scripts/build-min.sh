#!/bin/sh
# Optional extra-small build with nightly Rust: rebuilds std with
# panic=immediate-abort, which drops panic message formatting and the
# backtrace symbolizer (~30% of the binary). Panics then abort silently,
# so prefer the regular stable build for bug reports.
#
#   rustup toolchain install nightly --profile minimal -c rust-src
#   scripts/build-min.sh
set -eu
cd "$(dirname "$0")/.."
target=x86_64-unknown-linux-gnu
flags="-C target-cpu=x86-64 -C link-arg=-Wl,--gc-sections -C link-arg=-Wl,--as-needed -C link-arg=-Wl,--icf=all"
RUSTFLAGS="$flags -Zunstable-options -Cpanic=immediate-abort" \
    cargo +nightly build --release --locked --target $target -Zbuild-std=std,panic_abort ${FEATURES:+--features "$FEATURES"}
ls -l target/$target/release/haste

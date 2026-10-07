#!/bin/sh
# Prints RUSTFLAGS for size-optimised builds. Mirrors .cargo/config.toml and
# adds identical-code folding when rustc links with its bundled LLD (the
# default on x86_64 Linux since Rust 1.90; GNU ld does not support --icf).
set -eu
flags="-C target-cpu=x86-64 -C link-arg=-Wl,--gc-sections -C link-arg=-Wl,--as-needed"
minor=$(rustc --version | sed -n 's/^rustc 1\.\([0-9]*\).*/\1/p')
host=$(rustc -vV | sed -n 's/^host: //p')
if [ "$host" = x86_64-unknown-linux-gnu ] && [ "${minor:-0}" -ge 90 ]; then
    flags="$flags -C link-arg=-Wl,--icf=all"
fi
echo "$flags"

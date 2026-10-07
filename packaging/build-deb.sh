#!/bin/sh
# Builds dist/haste_<version>_<arch>.deb with dpkg-deb (no cargo-deb needed).
#
#   packaging/build-deb.sh
#   FEATURES="mpris tray" packaging/build-deb.sh
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
arch=$(dpkg --print-architecture)
maintainer=${DEB_MAINTAINER:-"Markus Zorgenfrei <markzorg@users.noreply.github.com>"}
feat=${FEATURES:+--features "$FEATURES"}

RUSTFLAGS="$(scripts/rustflags.sh)" cargo build --release --locked $feat

id=io.github.markzorg.Haste
root=target/deb/haste_${version}_${arch}
rm -rf "$root"
install -Dm755 target/release/haste "$root/usr/bin/haste"
install -Dm644 data/$id.desktop "$root/usr/share/applications/$id.desktop"
install -Dm644 data/$id.svg "$root/usr/share/icons/hicolor/scalable/apps/$id.svg"
install -Dm644 data/$id.metainfo.xml "$root/usr/share/metainfo/$id.metainfo.xml"
install -Dm644 LICENSE "$root/usr/share/doc/haste/copyright"

# Lowest glibc symbol version the binary needs.
glibc=$(objdump -T target/release/haste | sed -n 's/.*GLIBC_\([0-9.]*\).*/\1/p' | sort -uV | tail -n 1)

mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: haste
Version: $version
Section: sound
Priority: optional
Architecture: $arch
Depends: libgtk-4-1 (>= 4.12), libasound2t64, libc6 (>= $glibc)
Recommends: pipewire-alsa | libasound2-plugins
Installed-Size: $(du -sk "$root/usr" | cut -f1)
Maintainer: $maintainer
Homepage: https://github.com/markzorg/haste
Description: lightweight native GTK4 audio player
 A small music player with a sortable/searchable playlist, tags and cover
 art, shuffle/repeat, M3U/M3U8 import/export and session restore.
 Plays MP3, FLAC, Ogg Vorbis, WAV and AAC/M4A through ALSA (PipeWire or
 PulseAudio via their ALSA plugins).
CONTROL

# .desktop/icon caches are refreshed by dpkg triggers of desktop-file-utils
# and hicolor-icon-theme, so no maintainer scripts are needed.
mkdir -p dist
dpkg-deb --root-owner-group -Zxz --build "$root" dist/ >/dev/null
ls -l dist/haste_${version}_${arch}.deb

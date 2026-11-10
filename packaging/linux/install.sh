#!/bin/sh
# Installs Gleipnir from this folder, for you alone by default:
#   ./install.sh                     into ~/.local
#   sudo PREFIX=/usr/local ./install.sh   for everyone
#   ./install.sh --uninstall         takes it out again (with the same PREFIX)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"

refresh() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$apps" 2>/dev/null || true
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -t "$icons" 2>/dev/null || true
    fi
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$prefix/bin/gleipnir" "$apps/gleipnir.desktop" "$icons/scalable/apps/gleipnir.svg"
    for size in 128 256 512; do
        rm -f "$icons/${size}x${size}/apps/gleipnir.png"
    done
    refresh
    echo "Gleipnir removed from $prefix. Your settings stay in ~/.config/gleipnir, and installed tools in ~/.local/state/gleipnir."
    exit 0
fi

install -Dm755 "$here/bin/gleipnir" "$prefix/bin/gleipnir"
# The launcher names the program by its full path, as a desktop's own PATH may not include $prefix/bin.
launcher=$(mktemp)
sed "s|^Exec=gleipnir|Exec=$prefix/bin/gleipnir|" "$here/share/applications/gleipnir.desktop" > "$launcher"
install -Dm644 "$launcher" "$apps/gleipnir.desktop"
rm -f "$launcher"
install -Dm644 "$here/share/icons/hicolor/scalable/apps/gleipnir.svg" "$icons/scalable/apps/gleipnir.svg"
for size in 128 256 512; do
    install -Dm644 "$here/share/icons/hicolor/${size}x${size}/apps/gleipnir.png" \
        "$icons/${size}x${size}/apps/gleipnir.png"
done
refresh

echo "Gleipnir installed in $prefix."
case ":$PATH:" in
    *":$prefix/bin:"*) echo "Run it from your app launcher, or as: gleipnir path/to/project" ;;
    *) echo "Run it from your app launcher. To run it from a terminal, add $prefix/bin to your PATH." ;;
esac

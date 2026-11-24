#!/bin/sh
set -eu

bundle=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
bin_dir="$HOME/.local/bin"
data_dir=${XDG_DATA_HOME:-"$HOME/.local/share"}
app_dir="$data_dir/applications"
theme_dir="$data_dir/icons/hicolor"

mkdir -p "$bin_dir" "$app_dir"
for icon in "$bundle"/icons/*/stable-alm.png; do
    size=$(basename -- "$(dirname -- "$icon")")
    install -D -m 644 "$icon" "$theme_dir/$size/apps/stable-alm.png"
done

binary=$(mktemp "$bin_dir/.StableALM.XXXXXX")
launcher=$(mktemp "$app_dir/.stable-alm.XXXXXX")
trap 'if [ -e "$binary" ]; then unlink "$binary"; fi; if [ -e "$launcher" ]; then unlink "$launcher"; fi' EXIT
install -m 755 "$bundle/StableALM" "$binary"

# Quote the absolute executable path for desktop entries, including the string value's escaping.
exec_path=$(printf '%s' "$bin_dir/StableALM" | sed 's/\\/\\\\\\\\/g; s/["`$]/\\\\&/g; s/%/%%/g')
cat > "$launcher" <<EOF
[Desktop Entry]
Type=Application
Name=Stable ALM
Comment=Start the Stable ALM dashboard and worker
Exec="$exec_path"
Icon=stable-alm
Terminal=false
Categories=Office;Finance;
EOF
chmod 644 "$launcher"
mv -f -- "$binary" "$bin_dir/StableALM"
mv -f -- "$launcher" "$app_dir/stable-alm.desktop"

if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache --quiet --force --ignore-theme-index "$theme_dir"
fi
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$app_dir"
fi
printf '%s\n' 'Open Stable ALM from your application menu, then choose Open dashboard from its status bar icon.'

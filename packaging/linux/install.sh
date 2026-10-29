#!/bin/sh
# Adds wave9000 to this account's app menu, with its icon. Nothing
# outside your home folder is touched.
#
#   ./install.sh            add it (again, after moving this folder)
#   ./install.sh --remove   take it out of the menu
set -eu

here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
apps="$data/applications"
icons="$data/icons/hicolor/256x256/apps"

refresh() {
  command -v update-desktop-database >/dev/null && update-desktop-database -q "$apps" || true
  command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
}

if [ "${1:-}" = "--remove" ]; then
  rm -f "$apps/wave9000.desktop" "$icons/wave9000.png"
  refresh
  echo "wave9000 is out of the app menu. (This folder is untouched; delete it to remove the app.)"
  exit 0
fi

[ -x "$here/wave9000" ] || { echo "wave9000 isn't next to install.sh" >&2; exit 1; }
mkdir -p "$apps" "$icons"
cp "$here/wave9000.png" "$icons/wave9000.png"
# The path goes in quotes, escaped as a desktop file wants: \ " ` $ take
# a backslash (written twice, since the file's own strings unescape one),
# and % is doubled.
exec_path=$(printf '%s' "$here/wave9000" | sed -e 's/[\\"`$]/\\\\&/g' -e 's/%/%%/g')
WAVE_EXEC="Exec=\"$exec_path\" %f" awk '/^Exec=/ { print ENVIRON["WAVE_EXEC"]; next } { print }' \
  "$here/wave9000.desktop" > "$apps/wave9000.desktop"
chmod 644 "$apps/wave9000.desktop"
refresh
echo "wave9000 is in the app menu. Run ./install.sh again if you move this folder."

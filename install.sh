#!/bin/sh
# Installs Alias Calc from this checkout: the release binary, the screen
# reader library (loaded only while Orca runs), the desktop entry and the
# icons.
#
#   ./install.sh               build, then install into ~/.local
#   ./install.sh uninstall     remove what install put there
#
#   PREFIX=/usr DESTDIR=pkg ./install.sh       (packaging: aliasos-pkgs' PKGBUILD)
#   BIN=path/to/alias-calc LIB=path/to/libalias_calc_a11y.so ./install.sh
#                                              (install those, no build)
#
# PREFIX defaults to ~/.local; DESTDIR, if set, is put in front of every
# path (for staging), and then no caches are refreshed. The library goes in
# PREFIX/lib/alias-calc, one of the places src/a11y.rs looks for it.
set -eu

here=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}
destdir=${DESTDIR:-}
app=com.aliasproject.calc
library=libalias_calc_a11y.so

# The prefix goes into the desktop entry's Exec line, quoted, and must
# start the app from any directory: refuse a relative one, and characters
# that line (or sed) would read as something else.
case $prefix in
/*) ;;
*)
    printf 'install.sh: PREFIX must be an absolute path: %s\n' "$prefix" >&2
    exit 1
    ;;
esac
case $prefix in
*[\"\`\$\\%\&\|]* | *[[:cntrl:]]*)
    printf 'install.sh: PREFIX can'"'"'t hold any of " ` $ \\ %% & | or a control character: %s\n' "$prefix" >&2
    exit 1
    ;;
esac

bindir=$destdir$prefix/bin
libdir=$destdir$prefix/lib/alias-calc
appdir=$destdir$prefix/share/applications
icondir=$destdir$prefix/share/icons/hicolor

say() { printf '%s\n' "$*"; }

# Launchers read these caches; a user's ~/.local may have none. The icon
# cache is only refreshed if there is one already: a cache made here would
# hide icons that later installs add without refreshing it.
refresh() {
    [ -z "$destdir" ] || return 0
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$appdir" || true
    fi
    if [ -f "$icondir/icon-theme.cache" ] && command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache -q -f -t "$icondir" || true
    fi
}

# Copies $1 to $2 through a temporary name in the same folder, then renames
# it: a running alias-calc keeps the files it has open, and nothing ever
# sees half a file.
put() {
    mkdir -p "$(dirname "$2")"
    tmp=$(mktemp "$(dirname "$2")/.alias-calc.tmp.XXXXXX")
    trap 'rm -f "$tmp"' EXIT
    trap 'exit 130' INT TERM HUP
    if ! { cp "$1" "$tmp" && chmod "$3" "$tmp" && mv -f "$tmp" "$2"; }; then
        say "install.sh: couldn't put $2" >&2
        exit 1
    fi
    trap - EXIT INT TERM HUP
}

uninstall() {
    rm -f "$bindir/alias-calc" "$libdir/$library" "$appdir/$app.desktop" \
        "$icondir/scalable/apps/$app.svg" "$icondir/16x16/apps/$app.svg" \
        "$icondir/24x24/apps/$app.svg"
    rmdir "$libdir" 2>/dev/null || true
    # And any temporary file an install that was killed left.
    for left in "$bindir"/.alias-calc.tmp.?????? "$libdir"/.alias-calc.tmp.??????; do
        if [ -f "$left" ]; then rm -f "$left"; fi
    done
    refresh
    say "Removed Alias Calc from $destdir$prefix."
}

install_() {
    bin=${BIN:-}
    lib=${LIB:-}
    if [ -z "$bin" ]; then
        command -v cargo >/dev/null 2>&1 || {
            say "install.sh: cargo is needed to build (or set BIN and LIB to built files)" >&2
            exit 1
        }
        say "Building the release binary and the screen reader library..."
        # The paths cargo reports for what it built, wherever its target
        # dir puts them. (Kept, then read: a failed build stops here.)
        built=$(cd "$here" && cargo build --release --locked --workspace \
            --message-format=json-render-diagnostics)
        bin=$(printf '%s\n' "$built" | sed -n 's/.*"executable":"\([^"]*alias-calc\)".*/\1/p' | tail -n 1)
        lib=$(printf '%s\n' "$built" | grep -o "\"[^\"]*/$library\"" | tr -d '"' | tail -n 1)
        [ -n "$bin" ] || { say "install.sh: cargo didn't report a built alias-calc" >&2; exit 1; }
    fi
    [ -x "$bin" ] || { say "install.sh: no binary at $bin" >&2; exit 1; }
    [ -n "$lib" ] && [ -f "$lib" ] || { say "install.sh: no $library (set LIB)" >&2; exit 1; }

    put "$bin" "$bindir/alias-calc" 755
    put "$lib" "$libdir/$library" 755

    # The full path (quoted, for a PREFIX with spaces), so launchers find it
    # whether or not PREFIX/bin is on their PATH.
    mkdir -p "$appdir"
    sed "s|^Exec=alias-calc\$|Exec=\"$prefix/bin/alias-calc\"|" \
        "$here/packaging/$app.desktop" >"$appdir/$app.desktop"
    chmod 644 "$appdir/$app.desktop"

    # The mark at any size, and the pixel-grid files where it's drawn small.
    for target in scalable:$app.svg 16x16:$app-16.svg 24x24:$app-24.svg; do
        dir=${target%%:*}
        mkdir -p "$icondir/$dir/apps"
        cp "$here/packaging/icons/${target#*:}" "$icondir/$dir/apps/$app.svg"
        chmod 644 "$icondir/$dir/apps/$app.svg"
    done

    refresh
    say "Installed Alias Calc in $destdir$prefix:"
    say "  $bindir/alias-calc"
    say "  $libdir/$library (screen readers)"
    say "  $appdir/$app.desktop (\"Calculator\" in the launcher)"
}

case "${1:-install}" in
install) install_ ;;
uninstall) uninstall ;;
*)
    say "usage: ./install.sh [install|uninstall]" >&2
    exit 2
    ;;
esac

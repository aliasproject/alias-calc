# Alias Calc

A tiny, instant calculator for aliasOS. It starts faster, uses less memory and wakes the CPU less than omacalc and GNOME Calculator, does exact decimal math, and follows the live aliasOS theme.

Linear project: Alias Calc (P-ALI-28).

## Use

The keypad works with the mouse or the keyboard: `0-9 . + - * x / ^ %`, Enter or `=`, Backspace (held, it repeats), `C` or Escape to clear, and `S` to change sign. The numpad works with NumLock on or off.

- Ctrl+C (or Super+C, or Ctrl+Insert) copies the result without separators.
- Ctrl+V (or Super+V, or Shift+Insert) pastes a number, with or without separators: `1,234.5`, `1 234,5` and `12,34` all work. Anything that isn't a number is ignored.

Tab and Shift+Tab move a focus ring through the keys, and the arrow keys move it around the grid; Space presses the focused key. Text follows the aliasOS text size (`aliasos-display-text-size`).

With Orca running when it starts, the calculator works with the screen reader: each key is a named button, and the result is read out when it changes. Starting Orca later means reopening the calculator.

Colors follow the live aliasOS theme (`~/.local/state/aliasos/current/theme/colors.toml`), falling back to Omarchy's, and change as soon as the theme does.

## Build

```sh
cargo build --workspace --release
```

The binary is `target/release/alias-calc`. The aim is to link nothing beyond libc and `libxkbcommon` (for keyboard layouts), with a pure-Rust Wayland client.

Screen reader support is a separate library, `target/release/libalias_calc_a11y.so` (the `a11y/` crate, with AccessKit). The calculator loads it only when Orca is running, from beside its binary, from `lib/alias-calc/` beside the binary's `bin/` (where `install.sh` puts it), or from `/usr/lib/alias-calc/`, so without a screen reader it costs nothing. A package installs it to `/usr/lib/alias-calc/`, built with its own size-first profile (`cargo build --profile release-a11y -p alias-calc-a11y`, into `target/release-a11y/`): 1.9 MB instead of 2.7 MB.

## Install

```sh
./install.sh              # build, then install into ~/.local
./install.sh uninstall
```

It installs `bin/alias-calc`, the screen reader library in `lib/alias-calc/`, the desktop entry (`packaging/com.aliasproject.calc.desktop`, "Calculator" in the launcher) and the icons (`packaging/icons`). aliasOS packages it from the `alias-calc` PKGBUILD in aliasos-pkgs, which builds both first and then runs `PREFIX=/usr DESTDIR="$pkgdir" BIN=target/release/alias-calc LIB=target/release-a11y/libalias_calc_a11y.so ./install.sh`: with `BIN` and `LIB` set, nothing is built.

## Benchmark

Alias Calc must beat omacalc and GNOME Calculator on startup, memory and idle wakeups. In a Hyprland session, run:

```sh
OMACALC=/path/to/omacalc bench/run          # 10 runs each
bench/run --runs 3                           # quicker
bench/run --bin /usr/bin/alias-calc          # the installed package, not this checkout
bench/run --cold                             # drop the page cache before each launch (sudo)
```

For each app it times exec until Hyprland reports the window open, reads PSS, RSS and threads 2 s later, and counts wakeups over the next 5 idle seconds. It prints the medians as one table. The windows open on the current workspace, so leave the machine alone while it runs. omacalc builds from <https://github.com/omacom/omacalc> with `bin/build`.

Baselines (2026-11-25, RTX 4090, 4K at scale 1, median of 10):

| | Target | omacalc (e43fd0a) | GNOME Calculator 51.1 |
|---|---|---|---|
| Time to window mapped | < 20 ms | 148 ms | 241 ms |
| PSS | < 8 MB | 80 MB | 66 MB |
| Threads | 1 | 7 | 20 |
| Idle wakeups/s | 0.67 (Hyprland's ping only) | 3 | 348 |

Hyprland pings every window every 1.5 s to check that it still responds (`xdg_wm_base.ping`), so 0.67 wakeups/s is the floor for any window on Hyprland. Alias Calc wakes for nothing else while idle.

## Trace

`ALIAS_CALC_TRACE=1 alias-calc` prints startup timings and every wakeup to stderr:

```
[trace]   0.020 ms  connected
[trace]   0.105 ms  globals
[trace]   0.369 ms  configured
[trace]   0.608 ms  frame committed (320x480 px)
```

## Licence

MIT (`LICENSE`). The JetBrains Mono subsets compiled into the binary are under the SIL Open Font License 1.1 (`fonts/OFL.txt`).

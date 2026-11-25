# Alias Calc

A tiny, instant calculator for aliasOS. It starts faster, uses less memory and wakes the CPU less than omacalc and GNOME Calculator, does exact decimal math, and follows the live aliasOS theme.

Linear project: Alias Calc (P-ALI-28).

## Build

```sh
cargo build --release
```

The binary is `target/release/alias-calc`. It links only `wayland` and `libxkbcommon`.

## Benchmark

Alias Calc must beat omacalc and GNOME Calculator on startup, memory and idle wakeups. In a Hyprland session, run:

```sh
OMACALC=/path/to/omacalc bench/run          # 10 runs each
bench/run --runs 3                           # quicker
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

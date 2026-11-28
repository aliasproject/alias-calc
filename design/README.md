# Design tokens

`tokens.json` is the source for every Alias Calc design value (the app's Rust still repeats them as literals: see below): the colours of its one theme, JetBrains Mono and the type scales (the app's and the website's), and the spacing, radii, fixed sizes, motion and multiples the window and the website use. Each value has a note on where it's used. It uses the Design System artifact format: lists of `{name, value, usage}`, with colours given per theme.

**One theme.** Alias Calc follows the installed aliasOS theme at run time (`colors.toml`), and derives its key fills and label colours from it (`src/ui.rs`). The colours here are its built-in palette, the aliasOS default (`aliasos`), which the app falls back to (`Palette::default`) and which the website is drawn in. The contrast list checks that palette; an installed theme is made readable where it's drawn, by `readable()` in `src/ui.rs`, since this file can't check a theme it hasn't seen.

`build` turns it into the website's CSS. The generated file starts with a "Generated from design/tokens.json by design/build (design-gen 2.1.0) — don't edit" header:

| File | What it is |
|---|---|
| `site/tokens.css` | CSS custom properties on `:root`, one `--<name>` per token: colours (`--background`), `--font-mono`, each type style's `--<style>-size` in rem with its weight, line height and letter spacing, spacing, radii and sizes in px, motion in ms, and the multiples. The website's stylesheet reads them with `var()`. |

**The app doesn't read the tokens yet.** `src/ui.rs` and `src/window.rs` still hold these values as Rust literals (`PADDING`, `GAP`, `RADIUS`, `DISPLAY_SIZE`, `DEFAULT_SIZE`, `NOTICE_TIME`…), and they must match this file: change both together. Generating `src/tokens.rs` (`outputs.rust`) and setting those constants from it is a follow-up.

Some tokens are for the website alone (`"only": "web"`): the `web` type group, `space-1` to `space-24`, `radius-sm`, `content-max`, `rule-width` and `mono-advance`. The app's tokens aren't marked, because the website draws the calculator window from them too, so they reach the CSS and, once the app is wired up, the Rust.

## Changing a value

1. Edit `tokens.json`. Every token needs a `usage` note. Names are lowercase words joined by hyphens, and unique across all families except type styles, which are unique within their group and, in the CSS, across groups (`key-held` becomes `--key-held`; the `result` style becomes `--result-size`, `--result-weight` and `--result-line-height`). Units:
   - colours: a lowercase `#rrggbb` per theme, and an optional `alpha` (above 0, at most 1);
   - spacing, radius, size and type sizes: px (`"16px"`). The CSS writes type sizes in rem (px ÷ 16), so the website's text follows the reader's font size; the other lengths stay px;
   - type styles: `fontSize`, an optional `fontWeight` (100 to 900), and for the CSS an optional `lineHeight` (a multiple of the size, `1.25`, or px) and `letterSpacing` (`"-0.02em"`, px, or `"0"`);
   - motion: whole milliseconds (`"200ms"`, `"1.5s"`) or `"cubic-bezier(x1, y1, x2, y2)"`;
   - scale: a plain number above 0 (`1.3`).
2. If it's an app value, change the literal in `src/ui.rs` or `src/window.rs` to match.
3. Run `design/build` from anywhere in the repo. It needs only Cargo.
4. Commit `tokens.json` and `site/tokens.css` together.

On the website, use the custom properties (`var(--accent)`, `var(--space-6)`, `var(--web-body-size)`) instead of a literal value.

## The check

`design/build --check` changes nothing. Any other argument is a usage error (exit 2), so a typo never writes files. It exits 1 when:

- a generated file differs from what `tokens.json` produces (edited by hand, or the build wasn't run), or
- a pair in `contrast` falls below its `min`: 4.5 for text; 3 for large text, icons, control borders and focus rings. It uses the WCAG 2.2 relative-luminance formula and prints every pair with its ratio. `fgAlpha` blends the foreground over the background first, and a colour's own `alpha` multiplies the pair's. A translucent background (`bgAlpha`, or a colour with an alpha) names what's under it in `bgOver`: one colour, or a stack of layers, bottom first, each a colour or `{"color", "alpha"}` with the bottom one opaque. With two themes, a pair without `theme` is checked in each.

Decorative lines (1 px dividers) aren't controls, so they aren't in the list.

A pair with a `pending` note is a known miss waiting on a design decision. It also needs `theme` (with more than one theme), `ticket` (the ticket that fixes it, `ALI-…`) and `was` (the ratio it fails at when marked). The check prints it with its note and doesn't fail on it. It does fail if the pair gets worse than `was` (at two decimals), and once the pair passes, so the note can't outlive the fix. Remove all three keys when the fix lands. Alias Calc has none.

CI runs the check, and the generator's own fmt, clippy and tests, in `.github/workflows/design-tokens.yml`. The generator builds into `target/design-build`, apart from the app's builds.

## The generator

**design-gen 2.1.0.** That's the `version` in `design/Cargo.toml`; `design/build --help` prints it, and every generated file's header names it. Alias Calc is the first app on 2.1.0. Nitrate, Daybreak, Alias Notes and bosun are still on 2.0.0 until they take the new source; 2.1.0 writes their files unchanged apart from the header's version.

The generator has nothing app-specific: the outputs, their paths and every value come from `tokens.json`. It's its own Cargo workspace with no dependencies, so it builds the same inside any repo. What it reads, in full:

- `outputs`: at least one of `rust`, `swift` and `css`.
- `outputs.rust.path`: where the Rust module goes. Add `mod tokens;` (or whatever the file is called) and build the theme from its constants.
- `outputs.swift`: optional (`path`, `enum`, `note`, and `enums` to rename `type`, `spacing`, `radius`, `size`, `shadow`, `motion` or `scale`). With one theme, colours are SwiftUI `Color`s. With `colorType` (the app's typealias for UIColor/NSColor) and `appearance` (`{"dark": "<theme>", "light": "<theme>"}`), they're UIKit/AppKit colours that follow the appearance, through a helper named `dynamic` (so no colour can take that Swift name); an app with more than one theme needs these.
- `outputs.css` (new in 2.1.0): optional (`path`, `selector`, default `:root`, and `note`, written as a comment under the header). Each token written to CSS is a custom property named `--<token-name>`, under a comment with its usage note:
  - colours: `#rrggbb`, or `rgb(r g b / a)` for a colour with an alpha;
  - font families: `--font-<name>`, the quoted family and then the family's `css` fallback, if it has one;
  - type styles: for every style in every group written to CSS, `--<style>-size` in rem (px ÷ 16, trimmed: 56px is `3.5rem`), `--<style>-weight`, and `--<style>-line-height` (a multiple as written, px in rem) and `--<style>-letter-spacing` when the style gives them. Style names must be unique across the groups written to CSS, and no property may meet another token's (`--key-size` from a `key` style and a `key-size` token is an error);
  - spacing, radius and size: px; motion: `ms`, or the `cubic-bezier(...)`; scale: the plain number;
  - shadows: `Xpx Ypx Blurpx`, the spread in px if given, then `rgb(0 0 0 / opacity)`, or `var(--<colour>)` for a shadow drawn in a colour token, which must then be written to CSS too.

  With more than one theme, `outputs.css` needs `appearance` (`{"dark": "<theme>", "light": "<theme>"}`, as Swift's): the dark theme's colours go under the selector, and the light theme's, where they differ, in `@media (prefers-color-scheme: light)`. `selector` and a family's `css` can't hold braces, semicolons, backslashes, comment markers or unpaired quotes.
- Any family can be left out; an empty one isn't written. `type.families[].rust` names a font family's constant (default `FONT_<NAME>`); `type.families[].css` (new in 2.1.0) is its CSS fallback list (`"ui-monospace, monospace"`).
- `"only": "rust"`, `"swift"` or `"web"` on a colour, another family's token or (new in 2.1.0) a type group writes it for that output alone: `"web"` is the CSS output. A token without `only` goes to every output. A group with a `swift` name is written to Swift, so it can't be `only` another output. `lineHeight` and `letterSpacing` are written to the CSS only.
- A colour's `alpha`: one number (above 0, up to 1) for every theme, or an object with one per theme (`{"light": 0.12, "dark": 0.18}`; `1` where it's opaque in that theme). Rust writes `<NAME>_ALPHA` beside the colour in every theme, Swift an `.opacity()` (or the UIKit/AppKit colour's alpha), CSS an `rgb(r g b / a)`, and the contrast check composites it, multiplied by the pair's own alpha. A pair whose background (`bg`) is a translucent colour needs `bgOver`; a translucent foreground doesn't.
- `shadow`: `{"x", "y", "blur"}` in px, an optional `spread` in px (which may be negative), and either an `opacity` for the black or a `color` naming a colour token, drawn at that colour's own alpha (its `opacity` in Rust is then 1). SwiftUI shadows take neither a spread nor a colour, so with a Swift output such a shadow needs `"only": "rust"` or `"web"`.
- `scale`: unitless multiples, a number above 0 (`1.75`).
- `tables`: a list of an app's own families that the others can't express, each `{"name", "usage", "columns", "rows"}`. `columns` is a list of `{"name", "type", "usage"}`; `rows` is a list of `{"name", "value", "usage"}`, with `value` an object of column name to cell. Column types:
  - `color`: a colour token's name, written as that token's constant, so the contrast list checks the same value. It needs exactly one theme, and the colour must be opaque (no `alpha`; put an alpha in a `number` column) and written to Rust.
  - `time`: `"HH:MM"`, 00:00 to 23:59, written as minutes since midnight (`u16`).
  - `px`: `"16px"`, as `f32`.
  - `number`: any number, as `f32`.
  - `text`: a string without quotes, backslashes or control characters, as `&'static str`.

  A table is written to Rust only (it needs `outputs.rust`), as a module named after it with a `Row` struct (`name`, then a field per column), one constant per row and `ALL` in the file's order, each laid out as rustfmt lays it out, so `cargo fmt --check` passes on the generated file. Table names share the token names' space; rows and columns are unique within their table. A contrast pair's `fgAlpha` or `bgAlpha` can name a `number` cell as `"table.row.column"`.
- Numbers must fit an `f32` (`1e39px` is an error). Durations are whole milliseconds, written as digits with an optional decimal point (`"260ms"`, `"1.001s"`) and read exactly, not as floating point: any fraction of a millisecond is an error, however small (`"0.5ms"`, `"0.0000001ms"`) and however large the value.
- `tokens.json` is UTF-8 without a byte order mark; the build says so if it has one.

The check prints each ratio rounded to two decimals, except that a ratio just under its pair's `min` is cut instead (4.499:1 prints as 4.49, not 4.50). The printed number is the one compared with `min`, so a pair never passes by rounding up, as WCAG has it, and pending pairs compare the shown ratio with `was`: a pair at 2.996:1 with `min` 3 shows 2.99, so its `was` is 2.99.

## Keeping copies in step

Nitrate's copy (`aliasproject/nitrate`, `design/`) is the reference, and Daybreak, Alias Notes, bosun and Alias Calc copy its `build`, `Cargo.toml`, `Cargo.lock` and `src/` unchanged; only `tokens.json`, the generated files and the app's own sections of this README differ. 2.1.0 was written here first, so until Nitrate takes it, this copy is ahead of the reference. To change the generator:

1. Change Nitrate's copy first, with tests, and raise the version in `design/Cargo.toml`, `design/Cargo.lock` and the line above: a patch for a fix, a minor version for a new feature, a major one for a change to the token syntax or to an existing app's generated files.
2. Copy `src/`, `Cargo.toml` and `Cargo.lock` into the others, update the generator sections of their READMEs to match, run `design/build` in each and commit the regenerated files, whose header line names the new version.
3. To see whether a copy is in step, compare the version in its `design/Cargo.toml` with Nitrate's, or `diff -r` the two `design/src` folders.

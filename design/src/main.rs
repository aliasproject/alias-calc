//! Builds the app's themes from `design/tokens.json`.
//!
//!   design/build          write the generated files
//!   design/build --check  exit 1 if a generated file is out of date, or a
//!                         contrast pair in tokens.json misses its minimum
//!
//! Plain Rust with no dependencies (a small JSON reader is below), so any
//! repo with a Rust toolchain runs it with nothing to install. Nothing here
//! is specific to one app: the outputs, their paths and names, and every
//! value come from tokens.json. Outputs:
//!
//! - `outputs.rust`: a module of `0xRRGGBB` colour constants, one module per
//!   theme, the font families and the other families (type scale, spacing,
//!   radius, size, shadow, motion, scale and tables), for a gpui-kit app's
//!   theme to read.
//! - `outputs.swift` (optional): a SwiftUI enum of the colours, for a
//!   single-theme app; or, with `colorType` and `appearance`, UIKit and
//!   AppKit colours that follow dark and light mode, for an app with a
//!   theme for each.
//! - `outputs.css` (optional): CSS custom properties for a website, one
//!   `--<name>` per token, with a `prefers-color-scheme` block when there
//!   are two themes.
//!
//! Any one output is enough. Nitrate, Daybreak, Alias Notes, bosun and
//! Alias Calc run this same source; Nitrate's copy is the reference
//! (design/README.md, "Keeping copies in step").
//! `VERSION` names it in `--help` and in every generated file's header.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The generator's version, from Cargo.toml. Raise it with any change to
/// this source, so copies that differ never share a version.
const VERSION: &str = concat!("design-gen ", env!("CARGO_PKG_VERSION"));

const USAGE: &str = "usage: design/build [--check]\n\n  \
    (no flag)  write the generated files from design/tokens.json\n  \
    --check    change nothing; exit 1 if a generated file is stale or a contrast pair fails";

const HEADER: &str = concat!(
    "Generated from design/tokens.json by design/build (design-gen ",
    env!("CARGO_PKG_VERSION"),
    ") — don't edit."
);

fn main() -> ExitCode {
    let mut check = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{VERSION}\n\n{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("design/build: unknown argument \"{arg}\"\n\n{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let root = root();
    let source = root.join("design/tokens.json");
    let text = match std::fs::read_to_string(&source) {
        Ok(t) => t,
        Err(e) => return fail(&format!("can't read {}: {e}", source.display())),
    };
    let json = match parse(&text) {
        Ok(v) => v,
        Err(e) => return fail(&format!("design/tokens.json isn't valid JSON: {e}")),
    };
    let tokens = match Tokens::read(&json) {
        Ok(t) => t,
        Err(errors) => {
            return fail(&format!(
                "design/tokens.json is invalid:\n  {}",
                errors.join("\n  ")
            ))
        }
    };

    let outputs = tokens.outputs();
    if !check {
        for (path, contents) in &outputs {
            let file = root.join(path);
            if let Err(e) = write_file(&file, contents) {
                return fail(&format!("can't write {path}: {e}"));
            }
            println!("wrote {path}");
        }
        return ExitCode::SUCCESS;
    }

    let mut failed = false;
    println!("Generated files");
    for (path, contents) in &outputs {
        match std::fs::read_to_string(root.join(path)) {
            Ok(current) if current == *contents => println!("  ok     {path}"),
            Ok(_) => {
                println!("  STALE  {path}");
                failed = true;
            }
            Err(_) => {
                println!("  STALE  {path} (missing)");
                failed = true;
            }
        }
    }

    println!("\nContrast (WCAG 2.2 relative luminance)");
    let (pairs_failed, pending) = tokens.check_contrast(&mut std::io::stdout());
    failed |= pairs_failed;

    if failed {
        eprintln!(
            "\nFailed. Fix the tokens, then run design/build and commit the generated files."
        );
        return ExitCode::FAILURE;
    }
    print!("\nAll generated files are current and every contrast pair meets its minimum");
    if pending > 0 {
        println!(", except {pending} marked pending (known misses waiting on a decision).");
    } else {
        println!(".");
    }
    ExitCode::SUCCESS
}

/// Writes `contents` to `file`, making its folder first.
fn write_file(file: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, contents)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

/// The repo root: the folder above `design/`.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("design/ has a parent")
        .to_path_buf()
}

// ------------------------------------------------------------------ tokens

#[derive(Debug)]
struct Theme {
    id: String,
    name: String,
}

#[derive(Debug)]
struct ColorToken {
    name: String,
    /// One per theme, in the themes' order.
    values: Vec<Rgb>,
    /// Its opacity in each theme, in the themes' order: 1 unless it gives
    /// an `alpha`.
    alphas: Vec<f64>,
    /// Whether it gives an `alpha` (a tint, a shadow, a scrim): Rust then
    /// writes `<NAME>_ALPHA` beside it, even where a theme's alpha is 1.
    has_alpha: bool,
    usage: String,
    swift: Option<String>,
    only: Option<Only>,
}

impl ColorToken {
    /// See-through in at least one theme.
    fn translucent(&self) -> bool {
        self.alphas.iter().any(|a| *a < 1.0)
    }
}

/// A token written for one output only (`"only": "rust"`, `"swift"` or
/// `"web"`, the CSS output).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Only {
    Rust,
    Swift,
    Web,
}

fn in_rust(only: Option<Only>) -> bool {
    matches!(only, None | Some(Only::Rust))
}

fn in_swift(only: Option<Only>) -> bool {
    matches!(only, None | Some(Only::Swift))
}

fn in_css(only: Option<Only>) -> bool {
    matches!(only, None | Some(Only::Web))
}

/// A token's optional `"only"`: the one output it's written for.
fn read_only(t: &Value, at: &str, errors: &mut Vec<String>) -> Option<Only> {
    match t.get("only")?.str() {
        Some("rust") => Some(Only::Rust),
        Some("swift") => Some(Only::Swift),
        Some("web") => Some(Only::Web),
        _ => {
            errors.push(format!(
                "{at}: \"only\" must be \"rust\", \"swift\" or \"web\""
            ));
            None
        }
    }
}

#[derive(Debug)]
struct Family {
    name: String,
    value: String,
    usage: String,
    rust: Option<String>,
    /// The PostScript name's stem, for Apple (`InterDisplay` in
    /// `InterDisplay-Bold`).
    postscript: Option<String>,
    /// The CSS fallback after the quoted family (`ui-monospace, monospace`).
    css: Option<String>,
}

#[derive(Debug)]
struct TypeGroup {
    name: String,
    usage: String,
    /// The Swift enum it's written as; groups without one aren't written
    /// to Swift.
    swift: Option<String>,
    /// The one output its styles are written for.
    only: Option<Only>,
    styles: Vec<Style>,
}

#[derive(Debug)]
struct Style {
    name: String,
    family: Option<usize>,
    size: f64,
    weight: Option<u16>,
    /// CSS only: a multiple of the size, or a length.
    line_height: Option<LineHeight>,
    /// CSS only: in em or px.
    letter_spacing: Option<Spacing>,
    relative_to: Option<String>,
    usage: String,
    swift: Option<String>,
}

/// A style's `lineHeight`: a unitless multiple of its size (`1.25`), or a
/// length (`"20px"`).
#[derive(Debug, Clone, Copy, PartialEq)]
enum LineHeight {
    Multiple(f64),
    Px(f64),
}

/// A style's `letterSpacing`: `"-0.02em"` or `"0.5px"` (either may be
/// negative), or `"0"`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Spacing {
    Em(f64),
    Px(f64),
}

/// A spacing, radius or size token: a length in px (points on Apple).
#[derive(Debug)]
struct Length {
    name: String,
    px: f64,
    usage: String,
    swift: Option<String>,
    only: Option<Only>,
}

/// A shadow: offset, blur radius (as each platform's API takes it), an
/// optional spread, and either the black's opacity or a colour token to
/// draw it in (whose alpha, per theme, is then its opacity).
#[derive(Debug)]
struct ShadowToken {
    name: String,
    x: f64,
    y: f64,
    blur: f64,
    /// `None` when it isn't given; Rust then writes no `spread` field
    /// unless another shadow has one.
    spread: Option<f64>,
    opacity: f64,
    /// The colour token it's drawn in, instead of black.
    color: Option<usize>,
    usage: String,
    swift: Option<String>,
    only: Option<Only>,
}

#[derive(Debug, PartialEq)]
enum Motion {
    Millis(u64),
    Bezier([f64; 4]),
}

#[derive(Debug)]
struct MotionToken {
    name: String,
    value: Motion,
    usage: String,
    swift: Option<String>,
    only: Option<Only>,
}

/// A unitless multiple: a heading's size as a multiple of the body's, say.
#[derive(Debug)]
struct ScaleToken {
    name: String,
    value: f64,
    usage: String,
    swift: Option<String>,
    only: Option<Only>,
}

/// The families besides colour and font families.
#[derive(Debug, Default)]
struct More {
    groups: Vec<TypeGroup>,
    spacing: Vec<Length>,
    radius: Vec<Length>,
    size: Vec<Length>,
    shadow: Vec<ShadowToken>,
    motion: Vec<MotionToken>,
    scale: Vec<ScaleToken>,
}

#[derive(Debug)]
struct Pair {
    fg: usize,
    fg_alpha: f64,
    bg: usize,
    bg_alpha: f64,
    /// What's under a translucent bg: layers bottom first, each a colour
    /// and its alpha (the first is opaque).
    bg_over: Vec<(usize, f64)>,
    min: f64,
    /// Which themes it's checked in: every theme unless it names one.
    theme: Option<usize>,
    usage: String,
    pending: Option<Pending>,
}

#[derive(Debug)]
struct Pending {
    note: String,
    ticket: String,
    was: f64,
}

/// A table: an app's own family that the others can't express, such as
/// times of day each with its colours. Named rows, each with a value per
/// column; written to Rust as a module with a `Row` struct, one constant
/// per row and `ALL`.
#[derive(Debug)]
struct Table {
    name: String,
    usage: String,
    columns: Vec<Column>,
    rows: Vec<Row>,
}

#[derive(Debug)]
struct Column {
    name: String,
    kind: ColumnKind,
    usage: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ColumnKind {
    /// A colour token's name: the row refers to it, so the contrast list
    /// checks the same value.
    Color,
    /// `"HH:MM"`, as minutes since midnight.
    Time,
    /// `"12px"`.
    Px,
    /// Any finite number.
    Number,
    /// A string.
    Text,
}

const COLUMN_KINDS: [(&str, ColumnKind); 5] = [
    ("color", ColumnKind::Color),
    ("time", ColumnKind::Time),
    ("px", ColumnKind::Px),
    ("number", ColumnKind::Number),
    ("text", ColumnKind::Text),
];

#[derive(Debug, PartialEq)]
enum Cell {
    Color(usize),
    Minutes(u16),
    Num(f64),
    Text(String),
}

#[derive(Debug)]
struct Row {
    name: String,
    usage: String,
    /// One per column, in the columns' order.
    cells: Vec<Cell>,
}

#[derive(Debug)]
struct RustOut {
    path: String,
}

#[derive(Debug)]
struct CssOut {
    path: String,
    /// The rule the custom properties go in (`:root`).
    selector: String,
    note: Option<String>,
    /// With more than one theme, the themes shown in dark and light mode
    /// (`prefers-color-scheme`).
    appearance: Option<(usize, usize)>,
}

#[derive(Debug)]
struct SwiftOut {
    path: String,
    enum_name: String,
    note: Option<String>,
    /// The enums the other families are written as.
    enums: SwiftEnums,
    /// For UIKit and AppKit colours instead of SwiftUI ones: the app's
    /// name for UIColor/NSColor (a typealias it declares), and the themes
    /// shown in dark and light mode.
    platform: Option<PlatformColors>,
}

#[derive(Debug)]
struct PlatformColors {
    color_type: String,
    dark: usize,
    light: usize,
}

#[derive(Debug)]
struct SwiftEnums {
    type_scale: String,
    spacing: String,
    radius: String,
    size: String,
    shadow: String,
    motion: String,
    scale: String,
}

#[derive(Debug)]
struct Tokens {
    name: String,
    themes: Vec<Theme>,
    colors: Vec<ColorToken>,
    families: Vec<Family>,
    more: More,
    tables: Vec<Table>,
    contrast: Vec<Pair>,
    rust: Option<RustOut>,
    swift: Option<SwiftOut>,
    css: Option<CssOut>,
}

type Rgb = [u8; 3];

/// Token names: lowercase words joined by hyphens, which become valid
/// identifiers in Rust (`TEXT_SECONDARY`) and Swift (`textSecondary`).
fn good_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && !s.ends_with('-')
        && !s.contains("--")
}

fn good_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_')
}

fn good_ticket(s: &str) -> bool {
    s.strip_prefix("ALI-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6
        || !hex
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

impl Tokens {
    /// Reads and validates tokens.json: every problem found, not just the
    /// first, and never a panic.
    fn read(v: &Value) -> Result<Tokens, Vec<String>> {
        let mut errors = Vec::new();
        let mut err = |e: String| errors.push(e);

        let name = v.get("name").and_then(Value::str).unwrap_or("").to_string();
        if name.trim().is_empty() {
            err("name: the app's name is missing".into());
        }

        // Themes.
        let mut themes = Vec::new();
        let theme_list = v.get("color").and_then(|c| c.get("themes"));
        for (i, t) in theme_list
            .and_then(Value::arr)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let id = t.get("id").and_then(Value::str).unwrap_or("");
            if !good_ident(id) || id.bytes().any(|c| c.is_ascii_uppercase()) {
                err(format!(
                    "color.themes[{i}]: id \"{id}\" must be a lowercase identifier"
                ));
            }
            if themes.iter().any(|t: &Theme| t.id == id) {
                err(format!("color.themes[{i}]: \"{id}\" is listed twice"));
            }
            let name = t.get("name").and_then(Value::str).unwrap_or(id);
            themes.push(Theme {
                id: id.into(),
                name: name.into(),
            });
        }
        if themes.is_empty() {
            err("color.themes: list at least one theme ({\"id\", \"name\"})".into());
        }

        let mut seen = BTreeSet::new();
        // Problems found by helpers that take the list itself, merged in
        // once `err` is done with it.
        let mut errors_only = Vec::new();

        // Colours.
        let mut colors = Vec::new();
        let color_list = v.get("color").and_then(|c| c.get("tokens"));
        for (i, t) in color_list
            .and_then(Value::arr)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let name = t.get("name").and_then(Value::str).unwrap_or("").to_string();
            if !good_name(&name) {
                err(format!(
                    "color.tokens[{i}]: bad name \"{name}\" (lowercase words joined by hyphens)"
                ));
            }
            if !seen.insert(name.clone()) {
                err(format!("color: \"{name}\" is defined twice"));
            }
            let usage = t
                .get("usage")
                .and_then(Value::str)
                .unwrap_or("")
                .to_string();
            if usage.trim().is_empty() {
                err(format!("color: \"{name}\" has no usage note"));
            }
            let value = t.get("value");
            let mut values = Vec::new();
            for theme in &themes {
                let hex = value.and_then(|v| v.get(&theme.id)).and_then(Value::str);
                match hex.and_then(parse_hex) {
                    Some(rgb) => values.push(rgb),
                    None => {
                        err(format!(
                            "color: \"{name}\" needs a lowercase #rrggbb value for theme \"{}\"",
                            theme.id
                        ));
                        values.push([0, 0, 0]);
                    }
                }
            }
            if let Some(Value::Obj(entries)) = value {
                for (key, _) in entries {
                    if !themes.iter().any(|t| &t.id == key) {
                        err(format!(
                            "color: \"{name}\" has a value for unknown theme \"{key}\""
                        ));
                    }
                }
            } else if value.is_some_and(|v| !matches!(v, Value::Obj(_))) {
                err(format!(
                    "color: \"{name}\": value must be an object of theme id to #rrggbb"
                ));
            }
            let swift = t.get("swift").and_then(Value::str).map(str::to_string);
            if let Some(s) = &swift {
                if !good_ident(s) || SWIFT_KEYWORDS.contains(&s.as_str()) {
                    err(format!(
                        "color: \"{name}\": swift name \"{s}\" isn't a usable identifier"
                    ));
                }
            }
            // An opacity: one number for every theme, or an object of theme
            // id to number, one for each theme.
            let good_alpha = |a: f64| a > 0.0 && a <= 1.0;
            let alphas = match t.get("alpha") {
                None => vec![1.0; themes.len()],
                Some(Value::Num(a)) => {
                    if !good_alpha(*a) {
                        err(format!(
                            "color: \"{name}\": alpha must be above 0, up to 1"
                        ));
                    }
                    vec![*a; themes.len()]
                }
                Some(per) => themes
                    .iter()
                    .map(|theme| match per.get(&theme.id).and_then(Value::num) {
                        Some(a) if good_alpha(a) => a,
                        _ => {
                            err(format!(
                                "color: \"{name}\": alpha needs a number above 0, up to 1, for theme \"{}\" (or one number for all)",
                                theme.id
                            ));
                            1.0
                        }
                    })
                    .collect(),
            };
            if let Some(Value::Obj(entries)) = t.get("alpha") {
                for (key, _) in entries {
                    if !themes.iter().any(|t| &t.id == key) {
                        err(format!(
                            "color: \"{name}\" has an alpha for unknown theme \"{key}\""
                        ));
                    }
                }
            }
            let only = read_only(t, &format!("color: \"{name}\""), &mut errors_only);
            colors.push(ColorToken {
                name,
                values,
                alphas,
                has_alpha: t.get("alpha").is_some(),
                usage,
                swift,
                only,
            });
        }
        if colors.is_empty() {
            err("color.tokens: no colours".into());
        }

        // Font families.
        let mut families = Vec::new();
        let family_list = v.get("type").and_then(|t| t.get("families"));
        for (i, f) in family_list
            .and_then(Value::arr)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let name = f.get("name").and_then(Value::str).unwrap_or("").to_string();
            if !good_name(&name) {
                err(format!("type.families[{i}]: bad name \"{name}\""));
            }
            if !seen.insert(name.clone()) {
                err(format!("type.families: \"{name}\" is defined twice"));
            }
            let value = f
                .get("value")
                .and_then(Value::str)
                .unwrap_or("")
                .to_string();
            if value.trim().is_empty() || value.contains(['"', '\\']) {
                err(format!(
                    "type.families: \"{name}\" needs a family name as its value"
                ));
            }
            let usage = f
                .get("usage")
                .and_then(Value::str)
                .unwrap_or("")
                .to_string();
            if usage.trim().is_empty() {
                err(format!("type.families: \"{name}\" has no usage note"));
            }
            let rust = f.get("rust").and_then(Value::str).map(str::to_string);
            if let Some(r) = &rust {
                if !good_ident(r) {
                    err(format!(
                        "type.families: \"{name}\": rust name \"{r}\" isn't an identifier"
                    ));
                }
            }
            let postscript = f.get("postscript").and_then(Value::str).map(str::to_string);
            if let Some(ps) = &postscript {
                if !ps.bytes().all(|c| c.is_ascii_alphanumeric()) || ps.is_empty() {
                    err(format!(
                        "type.families: \"{name}\": postscript \"{ps}\" must be letters and digits"
                    ));
                }
            }
            let css = f.get("css").map(|c| c.str().unwrap_or("").to_string());
            if let Some(c) = &css {
                if !good_css_fallback(c) {
                    err(format!(
                        "type.families: \"{name}\": css must be a fallback list such as \"ui-monospace, monospace\" (no semicolons, braces, backslashes, comments or unpaired quotes)"
                    ));
                }
            }
            families.push(Family {
                name,
                value,
                usage,
                rust,
                postscript,
                css,
            });
        }

        errors.append(&mut errors_only);

        let tables = read_tables(v, &themes, &colors, &mut seen, &mut errors);

        // Contrast pairs.
        let color_index = |n: &str| colors.iter().position(|c| c.name == n);
        let mut contrast = Vec::new();
        for (i, p) in v
            .get("contrast")
            .and_then(Value::arr)
            .unwrap_or(&[])
            .iter()
            .enumerate()
        {
            let color = |key: &str, errors: &mut Vec<String>| {
                let n = p.get(key).and_then(Value::str).unwrap_or("");
                let found = color_index(n);
                if found.is_none() {
                    errors.push(format!("contrast[{i}]: unknown colour \"{n}\" for {key}"));
                }
                found
            };
            // A number, or a table's number cell as "table.row.column", so
            // the pair checks the alpha the app draws with.
            let alpha = |key: &str, errors: &mut Vec<String>| {
                let a = match p.get(key) {
                    None => return 1.0,
                    Some(Value::Num(a)) => Some(*a),
                    Some(Value::Str(r)) => match table_number(&tables, r) {
                        Ok(a) => Some(a),
                        Err(e) => {
                            errors.push(format!("contrast[{i}]: {key}: {e}"));
                            return 1.0;
                        }
                    },
                    Some(_) => None,
                };
                match a {
                    Some(a) if a > 0.0 && a <= 1.0 => a,
                    _ => {
                        errors.push(format!(
                            "contrast[{i}]: {key} must be a number above 0, up to 1, or a table's number cell (\"table.row.column\")"
                        ));
                        1.0
                    }
                }
            };
            let fg = color("fg", &mut errors);
            let bg = color("bg", &mut errors);
            let fg_alpha = alpha("fgAlpha", &mut errors);
            let bg_alpha = alpha("bgAlpha", &mut errors);
            let bg_over = bg_layers(
                p.get("bgOver"),
                &color_index,
                &format!("contrast[{i}]"),
                &mut errors,
            );
            // A bg is translucent by its bgAlpha, or by its token's alpha.
            let see_through = bg_alpha < 1.0 || bg.is_some_and(|b| colors[b].translucent());
            if see_through && p.get("bgOver").is_none() {
                errors.push(format!(
                    "contrast[{i}]: a translucent bg (bgAlpha, or a colour with an alpha) needs bgOver, the colour under it"
                ));
            }
            if !see_through && p.get("bgOver").is_some() {
                errors.push(format!(
                    "contrast[{i}]: bgOver is only for a translucent bg (bgAlpha below 1, or a colour with an alpha)"
                ));
            }
            if let Some(&(bottom, _)) = bg_over.first() {
                if colors[bottom].translucent() {
                    errors.push(format!(
                        "contrast[{i}]: bgOver's bottom layer must be opaque, and \"{}\" has an alpha",
                        colors[bottom].name
                    ));
                }
            }
            // WCAG's minimums. Each has at most two decimals, which
            // shown_ratio relies on.
            let min = p.get("min").and_then(Value::num).unwrap_or(0.0);
            if !MINIMUMS.contains(&min) {
                errors.push(format!("contrast[{i}]: min must be 3, 4.5 or 7"));
            }
            let theme = match p.get("theme").map(|t| t.str().unwrap_or("")) {
                None => None,
                Some(id) => {
                    let found = themes.iter().position(|t| t.id == id);
                    if found.is_none() {
                        errors.push(format!("contrast[{i}]: unknown theme \"{id}\""));
                    }
                    found
                }
            };
            let usage = p
                .get("usage")
                .and_then(Value::str)
                .unwrap_or("")
                .to_string();
            if usage.trim().is_empty() {
                errors.push(format!("contrast[{i}]: no usage note"));
            }
            let pending = match p.get("pending") {
                None => {
                    if p.get("ticket").is_some() || p.get("was").is_some() {
                        errors.push(format!(
                            "contrast[{i}]: \"ticket\" and \"was\" belong with \"pending\""
                        ));
                    }
                    None
                }
                Some(note) => {
                    let note = note.str().unwrap_or("").to_string();
                    if note.trim().is_empty() {
                        errors.push(format!(
                            "contrast[{i}]: \"pending\" needs a note (the proposed fix)"
                        ));
                    }
                    let ticket = p
                        .get("ticket")
                        .and_then(Value::str)
                        .unwrap_or("")
                        .to_string();
                    if !good_ticket(&ticket) {
                        errors.push(format!(
                            "contrast[{i}]: a pending pair needs the ticket that fixes it (\"ticket\": \"ALI-…\")"
                        ));
                    }
                    let was = p.get("was").and_then(Value::num).filter(|w| *w > 0.0);
                    if was.is_none() {
                        errors.push(format!(
                            "contrast[{i}]: a pending pair needs \"was\", the ratio it fails at today"
                        ));
                    }
                    if themes.len() > 1 && theme.is_none() {
                        errors.push(format!(
                            "contrast[{i}]: with more than one theme, a pending pair names its theme"
                        ));
                    }
                    Some(Pending {
                        note,
                        ticket,
                        was: was.unwrap_or(0.0),
                    })
                }
            };
            if let (Some(fg), Some(bg)) = (fg, bg) {
                contrast.push(Pair {
                    fg,
                    fg_alpha,
                    bg,
                    bg_alpha,
                    bg_over,
                    min,
                    theme,
                    usage,
                    pending,
                });
            }
        }

        let more = read_more(v, &families, &colors, &mut seen, &mut errors);

        // Outputs.
        let outputs = v.get("outputs");
        let path_of = |key: &str, errors: &mut Vec<String>| -> Option<String> {
            let o = outputs?.get(key)?;
            let path = o.get("path").and_then(Value::str).unwrap_or("");
            if path.is_empty() || path.starts_with('/') || path.split('/').any(|p| p == "..") {
                errors.push(format!("outputs.{key}.path must be a path inside the repo"));
                return None;
            }
            Some(path.to_string())
        };
        let rust = path_of("rust", &mut errors).map(|path| RustOut { path });
        let swift = path_of("swift", &mut errors).map(|path| {
            let o = outputs.and_then(|o| o.get("swift"));
            let enum_name = o
                .and_then(|o| o.get("enum"))
                .and_then(Value::str)
                .unwrap_or("Theme")
                .to_string();
            if !good_ident(&enum_name) {
                errors.push(format!(
                    "outputs.swift.enum \"{enum_name}\" isn't an identifier"
                ));
            }
            let names = o.and_then(|o| o.get("enums"));
            let mut name = |key: &str, default: &str| {
                let n = names
                    .and_then(|e| e.get(key))
                    .and_then(Value::str)
                    .unwrap_or(default)
                    .to_string();
                if !good_ident(&n) {
                    errors.push(format!(
                        "outputs.swift.enums.{key} \"{n}\" isn't an identifier"
                    ));
                }
                n
            };
            let enums = SwiftEnums {
                type_scale: name("type", "TypeScale"),
                spacing: name("spacing", "Spacing"),
                radius: name("radius", "Radius"),
                size: name("size", "Sizes"),
                shadow: name("shadow", "Shadows"),
                motion: name("motion", "Motion"),
                scale: name("scale", "Scale"),
            };
            // UIKit and AppKit colours: the app's typealias for them, and
            // which theme each appearance shows.
            let platform = o.and_then(|o| o.get("colorType")).map(|ct| {
                let color_type = ct.str().unwrap_or("").to_string();
                if !good_ident(&color_type) || SWIFT_KEYWORDS.contains(&color_type.as_str()) {
                    errors.push(format!(
                        "outputs.swift.colorType \"{color_type}\" isn't an identifier"
                    ));
                }
                let appearance = o.and_then(|o| o.get("appearance"));
                let mut theme_for = |mode: &str| {
                    let id = appearance
                        .and_then(|a| a.get(mode))
                        .and_then(Value::str)
                        .unwrap_or("");
                    let found = themes.iter().position(|t| t.id == id);
                    if found.is_none() {
                        errors.push(format!(
                            "outputs.swift.appearance.{mode} must name a theme (\"{id}\" isn't one)"
                        ));
                    }
                    found.unwrap_or(0)
                };
                let dark = theme_for("dark");
                let light = theme_for("light");
                PlatformColors {
                    color_type,
                    dark,
                    light,
                }
            });
            if platform.is_none() && themes.len() != 1 {
                errors.push(
                    "outputs.swift: with more than one theme, give colorType and appearance (dark and light), for colours that follow the appearance"
                        .into(),
                );
            }
            SwiftOut {
                path,
                enum_name,
                note: o
                    .and_then(|o| o.get("note"))
                    .and_then(Value::str)
                    .map(str::to_string),
                enums,
                platform,
            }
        });
        if swift.is_some() {
            for t in more.shadow.iter().filter(|t| in_swift(t.only)) {
                if t.color.is_some() || t.spread.is_some_and(|s| s != 0.0) {
                    errors.push(format!(
                        "shadow: \"{}\": SwiftUI shadows have no spread or theme colour, so the Swift output can't take this one (give it \"only\": \"rust\")",
                        t.name
                    ));
                }
            }
        }
        let css = path_of("css", &mut errors).map(|path| {
            let o = outputs.and_then(|o| o.get("css"));
            let selector = match o.and_then(|o| o.get("selector")) {
                None => ":root".to_string(),
                Some(sel) => {
                    let sel = sel.str().unwrap_or("").trim().to_string();
                    if !good_selector(&sel) {
                        errors.push(format!(
                            "outputs.css.selector \"{sel}\" must be a CSS selector without braces, semicolons, backslashes, at-rules, comments or unpaired quotes"
                        ));
                    }
                    sel
                }
            };
            let appearance = match o.and_then(|o| o.get("appearance")) {
                None => {
                    if themes.len() > 1 {
                        errors.push(
                            "outputs.css: with more than one theme, give appearance (dark and light), the themes shown by prefers-color-scheme"
                                .into(),
                        );
                    }
                    None
                }
                Some(a) => {
                    let mut theme_for = |mode: &str| {
                        let id = a.get(mode).and_then(Value::str).unwrap_or("");
                        let found = themes.iter().position(|t| t.id == id);
                        if found.is_none() {
                            errors.push(format!(
                                "outputs.css.appearance.{mode} must name a theme (\"{id}\" isn't one)"
                            ));
                        }
                        found.unwrap_or(0)
                    };
                    let dark = theme_for("dark");
                    let light = theme_for("light");
                    Some((dark, light))
                }
            };
            let note = o
                .and_then(|o| o.get("note"))
                .and_then(Value::str)
                .map(str::to_string);
            CssOut {
                path,
                selector,
                note,
                appearance,
            }
        });
        if css.is_some() {
            for t in more.shadow.iter().filter(|t| in_css(t.only)) {
                if t.color.is_some_and(|c| !in_css(colors[c].only)) {
                    errors.push(format!(
                        "shadow: \"{}\": its colour isn't written to CSS, so the CSS output can't name it (give the shadow \"only\", or the colour none)",
                        t.name
                    ));
                }
            }
        }
        if rust.is_none() && swift.is_none() && css.is_none() {
            errors.push("outputs: name at least one output (rust, swift, css)".into());
        }
        if !tables.is_empty() && rust.is_none() {
            errors.push("tables: tables are written to Rust only; they need outputs.rust".into());
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        let tokens = Tokens {
            name,
            themes,
            colors,
            families,
            more,
            tables,
            contrast,
            rust,
            swift,
            css,
        };
        let errors = tokens.check_identifiers();
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(tokens)
    }

    fn outputs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Some(r) = &self.rust {
            out.push((r.path.clone(), self.rust_module()));
        }
        if let Some(s) = &self.swift {
            out.push((s.path.clone(), self.swift_file(s)));
        }
        if let Some(c) = &self.css {
            out.push((c.path.clone(), self.css_file(c)));
        }
        out
    }

    fn rust_module(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "//! {HEADER}");
        let _ = writeln!(o, "//!");
        let _ = writeln!(
            o,
            "//! {}'s design tokens. Colours are sRGB `0xRRGGBB`, one module per theme.",
            self.name
        );
        if self.colors.iter().any(|c| c.has_alpha && in_rust(c.only)) {
            o.push_str("//! A translucent colour's opacity is `<NAME>_ALPHA` beside it.\n");
        }
        o.push_str("\n#![allow(dead_code)]\n");
        for (i, theme) in self.themes.iter().enumerate() {
            let _ = writeln!(o, "\n/// The {} theme's colours.", theme.name);
            let _ = writeln!(o, "pub mod {} {{", theme.id);
            let colors = self.colors.iter().filter(|c| in_rust(c.only));
            for (n, c) in colors.enumerate() {
                if n > 0 {
                    o.push('\n');
                }
                let _ = writeln!(o, "    /// {}", doc(&c.usage));
                let [r, g, b] = c.values[i];
                let id = screaming(&c.name);
                let _ = writeln!(o, "    pub const {id}: u32 = 0x{r:02X}{g:02X}{b:02X};");
                // A colour with an alpha has one in every theme.
                if c.has_alpha {
                    let _ = writeln!(
                        o,
                        "    /// The opacity {} is drawn at.\n    pub const {id}_ALPHA: f32 = {};",
                        c.name,
                        rs_num(c.alphas[i])
                    );
                }
            }
            o.push_str("}\n");
        }
        for f in &self.families {
            let name = f
                .rust
                .clone()
                .unwrap_or_else(|| format!("FONT_{}", screaming(&f.name)));
            let _ = writeln!(o, "\n/// {}", doc(&f.usage));
            let _ = writeln!(o, "pub const {name}: &str = \"{}\";", f.value);
        }
        self.rust_more(&mut o);
        self.rust_tables(&mut o);
        o
    }
    fn rust_tables(&self, o: &mut String) {
        for table in &self.tables {
            let module = table.name.replace('-', "_");
            let _ = writeln!(o, "\n/// {}", doc(&table.usage));
            let _ = writeln!(o, "pub mod {module} {{");
            o.push_str("    /// One row of the table.\n");
            o.push_str("    #[derive(Clone, Copy, Debug, PartialEq)]\n");
            o.push_str("    pub struct Row {\n");
            o.push_str("        /// The row's name in tokens.json.\n");
            o.push_str("        pub name: &'static str,\n");
            for c in &table.columns {
                let (ty, unit) = match c.kind {
                    ColumnKind::Color => ("u32", " `0xRRGGBB`."),
                    ColumnKind::Time => ("u16", " Minutes since midnight."),
                    ColumnKind::Px => ("f32", " In px."),
                    ColumnKind::Number => ("f32", ""),
                    ColumnKind::Text => ("&'static str", ""),
                };
                let _ = writeln!(o, "        /// {}{unit}", doc(&c.usage));
                let _ = writeln!(o, "        pub {}: {ty},", c.name.replace('-', "_"));
            }
            o.push_str("    }\n");
            for row in &table.rows {
                let _ = writeln!(o, "\n    /// {}", doc(&row.usage));
                let mut fields = vec![format!("name: \"{}\"", row.name)];
                for (c, cell) in table.columns.iter().zip(&row.cells) {
                    let value = match cell {
                        Cell::Color(i) => format!(
                            "super::{}::{}",
                            self.themes[0].id,
                            screaming(&self.colors[*i].name)
                        ),
                        Cell::Minutes(m) => m.to_string(),
                        Cell::Num(x) => rs_num(*x),
                        Cell::Text(t) => format!("\"{t}\""),
                    };
                    fields.push(format!("{}: {value}", c.name.replace('-', "_")));
                }
                o.push_str(&row_literal(&screaming(&row.name), &fields));
            }
            let names: Vec<String> = table.rows.iter().map(|r| screaming(&r.name)).collect();
            o.push_str("\n    /// Every row, in tokens.json's order.\n");
            o.push_str(&all_rows(&names));
            o.push_str("}\n");
        }
    }

    fn rust_more(&self, o: &mut String) {
        let m = &self.more;
        let groups: Vec<&TypeGroup> = m.groups.iter().filter(|g| in_rust(g.only)).collect();
        if !groups.is_empty() {
            o.push_str("\n/// Font sizes in px, by group; `<NAME>_WEIGHT` is the weight (400 regular, 700 bold).\n");
            o.push_str("pub mod type_scale {\n");
            for (g, group) in groups.iter().enumerate() {
                if g > 0 {
                    o.push('\n');
                }
                let _ = writeln!(o, "    /// {}", doc(&group.usage));
                let _ = writeln!(o, "    pub mod {} {{", group.name.replace('-', "_"));
                for (n, st) in group.styles.iter().enumerate() {
                    if n > 0 {
                        o.push('\n');
                    }
                    let id = screaming(&st.name);
                    let _ = writeln!(o, "        /// {}", doc(&st.usage));
                    let _ = writeln!(o, "        pub const {id}: f32 = {};", rs_num(st.size));
                    if let Some(w) = st.weight {
                        let _ = writeln!(
                            o,
                            "        pub const {id}_WEIGHT: f32 = {};",
                            rs_num(f64::from(w))
                        );
                    }
                }
                o.push_str("    }\n");
            }
            o.push_str("}\n");
        }
        for (module, what, list) in [
            ("spacing", "Spacing", &m.spacing),
            ("radius", "Corner radii", &m.radius),
            ("size", "Fixed sizes", &m.size),
        ] {
            let list: Vec<&Length> = list.iter().filter(|t| in_rust(t.only)).collect();
            if list.is_empty() {
                continue;
            }
            let _ = writeln!(o, "\n/// {what}, in px.");
            let _ = writeln!(o, "pub mod {module} {{");
            for (n, t) in list.iter().enumerate() {
                if n > 0 {
                    o.push('\n');
                }
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                let _ = writeln!(
                    o,
                    "    pub const {}: f32 = {};",
                    screaming(&t.name),
                    rs_num(t.px)
                );
            }
            o.push_str("}\n");
        }
        let shadows: Vec<&ShadowToken> = m.shadow.iter().filter(|t| in_rust(t.only)).collect();
        if !shadows.is_empty() {
            let spread = shadows.iter().any(|t| t.spread.is_some());
            let colored = shadows.iter().any(|t| t.color.is_some());
            if spread {
                o.push_str(
                    "\n/// Shadows: offset, blur and spread in px, and the black's opacity.\n",
                );
            } else {
                o.push_str(
                    "\n/// Black shadows: offset and blur in px, and the black's opacity.\n",
                );
            }
            if colored {
                o.push_str("/// A shadow drawn in a theme colour says which; its opacity is 1, and\n/// the colour's own alpha applies.\n");
            }
            o.push_str("pub mod shadow {\n");
            o.push_str("    #[derive(Clone, Copy, Debug, PartialEq)]\n");
            o.push_str("    pub struct Shadow {\n        pub x: f32,\n        pub y: f32,\n        pub blur: f32,\n");
            if spread {
                o.push_str("        pub spread: f32,\n");
            }
            o.push_str("        pub opacity: f32,\n    }\n");
            for t in shadows {
                let _ = writeln!(o, "\n    /// {}", doc(&t.usage));
                if let Some(c) = t.color {
                    let _ = writeln!(
                        o,
                        "    /// Drawn in the theme's `{}` colour.",
                        self.colors[c].name
                    );
                }
                let _ = write!(
                    o,
                    "    pub const {}: Shadow = Shadow {{\n        x: {},\n        y: {},\n        blur: {},\n",
                    screaming(&t.name),
                    rs_num(t.x),
                    rs_num(t.y),
                    rs_num(t.blur),
                );
                if spread {
                    let _ = writeln!(o, "        spread: {},", rs_num(t.spread.unwrap_or(0.0)));
                }
                let _ = writeln!(o, "        opacity: {},\n    }};", rs_num(t.opacity));
            }
            o.push_str("}\n");
        }
        let motion: Vec<&MotionToken> = m.motion.iter().filter(|t| in_rust(t.only)).collect();
        if !motion.is_empty() {
            o.push_str("\n/// Durations, and easings as cubic Béziers `[x1, y1, x2, y2]`.\n");
            o.push_str("pub mod motion {\n    use std::time::Duration;\n");
            for t in motion {
                let _ = writeln!(o, "\n    /// {}", doc(&t.usage));
                let id = screaming(&t.name);
                match t.value {
                    Motion::Millis(ms) => {
                        let _ = writeln!(
                            o,
                            "    pub const {id}: Duration = Duration::from_millis({ms});"
                        );
                    }
                    Motion::Bezier(b) => {
                        let _ = writeln!(
                            o,
                            "    pub const {id}: [f32; 4] = [{}];",
                            b.map(rs_num).join(", ")
                        );
                    }
                }
            }
            o.push_str("}\n");
        }
        let scale: Vec<&ScaleToken> = m.scale.iter().filter(|t| in_rust(t.only)).collect();
        if !scale.is_empty() {
            o.push_str("\n/// Unitless multiples.\n");
            o.push_str("pub mod scale {\n");
            for (n, t) in scale.iter().enumerate() {
                if n > 0 {
                    o.push('\n');
                }
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                let _ = writeln!(
                    o,
                    "    pub const {}: f32 = {};",
                    screaming(&t.name),
                    rs_num(t.value)
                );
            }
            o.push_str("}\n");
        }
    }

    fn swift_file(&self, s: &SwiftOut) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "// {HEADER}");
        if let Some(note) = &s.note {
            let _ = writeln!(o, "// {}", doc(note));
        }
        let colors: Vec<&ColorToken> = self.colors.iter().filter(|c| in_swift(c.only)).collect();
        let hex = |c: &ColorToken, t: usize| {
            let [r, g, b] = c.values[t];
            format!("0x{r:02X}{g:02X}{b:02X}")
        };
        if let Some(p) = &s.platform {
            let ct = &p.color_type;
            o.push_str("\nimport SwiftUI\n#if canImport(UIKit)\nimport UIKit\n#else\nimport AppKit\n#endif\n\n");
            let _ = writeln!(o, "enum {} {{", s.enum_name);
            let _ = writeln!(
                o,
                "    /// `dark` in dark mode, `light` in light, each at its alpha.\n    \
                 private static func dynamic(\n        _ dark: UInt32, _ light: UInt32, alpha: (dark: CGFloat, light: CGFloat) = (1, 1)\n    \
                 ) -> {ct} {{\n        \
                 #if canImport(UIKit)\n        \
                 UIColor {{ traits in\n            \
                 traits.userInterfaceStyle == .light\n                \
                 ? UIColor(hex: light, alpha: alpha.light) : UIColor(hex: dark, alpha: alpha.dark)\n        \
                 }}\n        \
                 #else\n        \
                 NSColor(name: nil) {{ appearance in\n            \
                 appearance.bestMatch(from: [.darkAqua, .aqua]) == .aqua\n                \
                 ? NSColor(hex: light, alpha: alpha.light) : NSColor(hex: dark, alpha: alpha.dark)\n        \
                 }}\n        \
                 #endif\n    }}"
            );
            for c in &colors {
                let name = swift_id(&c.name, &c.swift);
                let (dark, light) = (hex(c, p.dark), hex(c, p.light));
                let (da, la) = (c.alphas[p.dark], c.alphas[p.light]);
                let _ = writeln!(o, "\n    /// {}", doc(&c.usage));
                if dark == light && da == la {
                    let alpha = if da < 1.0 {
                        format!(", alpha: {}", swift_num(da))
                    } else {
                        String::new()
                    };
                    let _ = writeln!(o, "    static let {name} = {ct}(hex: {dark}{alpha})");
                } else {
                    let alpha = if da < 1.0 || la < 1.0 {
                        format!(", alpha: ({}, {})", swift_num(da), swift_num(la))
                    } else {
                        String::new()
                    };
                    let _ = writeln!(o, "    static let {name} = dynamic({dark}, {light}{alpha})");
                }
            }
            o.push_str("}\n\n");
            let _ = writeln!(
                o,
                "extension {ct} {{\n    \
                 convenience init(hex: UInt32, alpha: CGFloat = 1) {{\n        \
                 let (r, g, b) = (\n            \
                 CGFloat((hex >> 16) & 0xFF) / 255, CGFloat((hex >> 8) & 0xFF) / 255, CGFloat(hex & 0xFF) / 255\n        \
                 )\n        \
                 #if canImport(UIKit)\n        \
                 self.init(red: r, green: g, blue: b, alpha: alpha)\n        \
                 #else\n        \
                 self.init(srgbRed: r, green: g, blue: b, alpha: alpha)\n        \
                 #endif\n    }}\n\n    \
                 /// The colour for SwiftUI.\n    \
                 var color: Color {{\n        \
                 #if canImport(UIKit)\n        \
                 Color(uiColor: self)\n        \
                 #else\n        \
                 Color(nsColor: self)\n        \
                 #endif\n    }}\n}}"
            );
        } else {
            o.push_str("\nimport SwiftUI\n\n");
            let _ = writeln!(o, "enum {} {{", s.enum_name);
            for c in &colors {
                let name = swift_id(&c.name, &c.swift);
                let alpha = if c.alphas[0] < 1.0 {
                    format!(".opacity({})", swift_num(c.alphas[0]))
                } else {
                    String::new()
                };
                let _ = writeln!(o, "    /// {}", doc(&c.usage));
                let _ = writeln!(
                    o,
                    "    static let {name} = Color(hex: {}){alpha}",
                    hex(c, 0)
                );
            }
            o.push_str("}\n\n");
            o.push_str(
                "extension Color {\n    init(hex: UInt32) {\n        self.init(\n            \
                 red: Double(hex >> 16 & 0xFF) / 255, green: Double(hex >> 8 & 0xFF) / 255,\n            \
                 blue: Double(hex & 0xFF) / 255)\n    }\n}\n",
            );
        }
        self.swift_more(s, &mut o);
        o
    }

    fn swift_more(&self, s: &SwiftOut, o: &mut String) {
        let m = &self.more;
        let e = &s.enums;
        let groups: Vec<&TypeGroup> = m
            .groups
            .iter()
            .filter(|g| g.swift.is_some() && in_swift(g.only))
            .collect();
        if !groups.is_empty() {
            let _ = writeln!(o, "\n/// The type scale, by platform. Sizes are points.");
            let _ = writeln!(o, "enum {} {{", e.type_scale);
            for (g, group) in groups.iter().enumerate() {
                if g > 0 {
                    o.push('\n');
                }
                let _ = writeln!(o, "    /// {}", doc(&group.usage));
                let _ = writeln!(o, "    enum {} {{", group.swift.as_deref().unwrap_or(""));
                for st in &group.styles {
                    // Validated: a Swift group's styles have both.
                    let ps = st
                        .family
                        .and_then(|f| self.families[f].postscript.as_deref())
                        .unwrap_or("");
                    let weight = WEIGHTS
                        .iter()
                        .find(|(n, _)| Some(*n) == st.weight)
                        .map_or("Regular", |(_, w)| w);
                    let relative = st
                        .relative_to
                        .as_ref()
                        .map(|r| format!(", relativeTo: .{r}"))
                        .unwrap_or_default();
                    let _ = writeln!(o, "        /// {}", doc(&st.usage));
                    let _ = writeln!(
                        o,
                        "        static let {} = Font.custom(\"{ps}-{weight}\", size: {}{relative})",
                        swift_id(&st.name, &st.swift),
                        swift_num(st.size)
                    );
                }
                o.push_str("    }\n");
            }
            o.push_str("}\n");
        }
        for (name, what, list) in [
            (&e.spacing, "Spacing", &m.spacing),
            (&e.radius, "Corner radii", &m.radius),
            (&e.size, "Fixed sizes", &m.size),
        ] {
            let list: Vec<&Length> = list.iter().filter(|t| in_swift(t.only)).collect();
            if list.is_empty() {
                continue;
            }
            let _ = writeln!(o, "\n/// {what}, in points.");
            let _ = writeln!(o, "enum {name} {{");
            for t in list {
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                let _ = writeln!(
                    o,
                    "    static let {}: CGFloat = {}",
                    swift_id(&t.name, &t.swift),
                    swift_num(t.px)
                );
            }
            o.push_str("}\n");
        }
        let shadows: Vec<&ShadowToken> = m.shadow.iter().filter(|t| in_swift(t.only)).collect();
        if !shadows.is_empty() {
            let _ = writeln!(
                o,
                "\n/// A black shadow: offset, blur radius and the black's opacity."
            );
            let _ = writeln!(o, "struct ShadowToken {{\n    let x: CGFloat\n    let y: CGFloat\n    let radius: CGFloat\n    let opacity: Double\n}}");
            o.push_str(
                "\nextension View {\n    /// The token's black shadow; none while `visible` is false.\n    \
                 func shadow(_ s: ShadowToken, visible: Bool = true) -> some View {\n        \
                 shadow(color: .black.opacity(visible ? s.opacity : 0), radius: s.radius, x: s.x, y: s.y)\n    \
                 }\n}\n",
            );
            let _ = writeln!(o, "\nenum {} {{", e.shadow);
            for t in shadows {
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                let _ = writeln!(
                    o,
                    "    static let {} = ShadowToken(x: {}, y: {}, radius: {}, opacity: {})",
                    swift_id(&t.name, &t.swift),
                    swift_num(t.x),
                    swift_num(t.y),
                    swift_num(t.blur),
                    swift_num(t.opacity)
                );
            }
            o.push_str("}\n");
        }
        let motion: Vec<&MotionToken> = m.motion.iter().filter(|t| in_swift(t.only)).collect();
        if !motion.is_empty() {
            let _ = writeln!(
                o,
                "\n/// Durations in seconds, and easings as timing curves."
            );
            let _ = writeln!(o, "enum {} {{", e.motion);
            for t in motion {
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                match t.value {
                    Motion::Millis(ms) => {
                        let _ = writeln!(
                            o,
                            "    static let {}: Double = {}",
                            swift_id(&t.name, &t.swift),
                            seconds(ms)
                        );
                    }
                    Motion::Bezier(b) => {
                        let _ = writeln!(
                            o,
                            "    static func {}(duration: Double) -> Animation {{\n        .timingCurve({}, duration: duration)\n    }}",
                            swift_id(&t.name, &t.swift),
                            b.map(swift_num).join(", ")
                        );
                    }
                }
            }
            o.push_str("}\n");
        }
        let scale: Vec<&ScaleToken> = m.scale.iter().filter(|t| in_swift(t.only)).collect();
        if !scale.is_empty() {
            let _ = writeln!(o, "\n/// Unitless multiples.");
            let _ = writeln!(o, "enum {} {{", e.scale);
            for t in scale {
                let _ = writeln!(o, "    /// {}", doc(&t.usage));
                let _ = writeln!(
                    o,
                    "    static let {}: CGFloat = {}",
                    swift_id(&t.name, &t.swift),
                    swift_num(t.value)
                );
            }
            o.push_str("}\n");
        }
    }

    /// The CSS custom properties: one `--<name>` per token written to CSS,
    /// in the selector's rule. With two themes the dark one goes there, and
    /// the light one's differing colours in a `prefers-color-scheme: light`
    /// block.
    fn css_file(&self, c: &CssOut) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "/* {HEADER} */");
        if let Some(note) = &c.note {
            let _ = writeln!(o, "/* {} */", css_comment(note));
        }
        let (dark, light) = c.appearance.unwrap_or((0, 0));
        let colors: Vec<&ColorToken> = self.colors.iter().filter(|t| in_css(t.only)).collect();
        // Each section: a heading, then its (usage, property, value) lines.
        type Line = (String, String, String);
        let mut sections: Vec<(String, Vec<Line>)> = Vec::new();
        let colour_heading = if c.appearance.is_some() && dark != light {
            format!("Colours: {} (dark mode).", self.themes[dark].name)
        } else {
            format!("Colours: {}.", self.themes[dark].name)
        };
        sections.push((
            colour_heading,
            colors
                .iter()
                .map(|t| (t.usage.clone(), t.name.clone(), css_color(t, dark)))
                .collect(),
        ));
        sections.push((
            "Font families.".into(),
            self.families
                .iter()
                .map(|f| {
                    let value = match &f.css {
                        Some(fallback) => format!("\"{}\", {}", f.value, fallback.trim()),
                        None => format!("\"{}\"", f.value),
                    };
                    (f.usage.clone(), format!("font-{}", f.name), value)
                })
                .collect(),
        ));
        let m = &self.more;
        for g in m.groups.iter().filter(|g| in_css(g.only)) {
            let mut lines = Vec::new();
            for st in &g.styles {
                lines.push((st.usage.clone(), format!("{}-size", st.name), rem(st.size)));
                // The usage note goes on the first line only.
                let mut more = |suffix: &str, value: String| {
                    lines.push((String::new(), format!("{}-{suffix}", st.name), value));
                };
                if let Some(w) = st.weight {
                    more("weight", w.to_string());
                }
                match st.line_height {
                    Some(LineHeight::Multiple(n)) => more("line-height", css_num(n)),
                    Some(LineHeight::Px(px)) => more("line-height", rem(px)),
                    None => {}
                }
                match st.letter_spacing {
                    Some(Spacing::Em(0.0)) => more("letter-spacing", "0".into()),
                    Some(Spacing::Em(n)) => more("letter-spacing", format!("{}em", css_num(n))),
                    Some(Spacing::Px(n)) => more("letter-spacing", format!("{}px", css_num(n))),
                    None => {}
                }
            }
            sections.push((
                format!("Type, {}: sizes in rem (px ÷ 16). {}", g.name, g.usage),
                lines,
            ));
        }
        for (what, list) in [
            ("Spacing.", &m.spacing),
            ("Corner radii.", &m.radius),
            ("Fixed sizes.", &m.size),
        ] {
            sections.push((
                what.into(),
                list.iter()
                    .filter(|t| in_css(t.only))
                    .map(|t| {
                        (
                            t.usage.clone(),
                            t.name.clone(),
                            format!("{}px", css_num(t.px)),
                        )
                    })
                    .collect(),
            ));
        }
        sections.push((
            "Shadows.".into(),
            m.shadow
                .iter()
                .filter(|t| in_css(t.only))
                .map(|t| {
                    let mut v = format!(
                        "{}px {}px {}px",
                        css_num(t.x),
                        css_num(t.y),
                        css_num(t.blur)
                    );
                    if let Some(spread) = t.spread {
                        let _ = write!(v, " {}px", css_num(spread));
                    }
                    match t.color {
                        // Validated: the colour is written to CSS too.
                        Some(i) => {
                            let _ = write!(v, " var(--{})", self.colors[i].name);
                        }
                        None => {
                            let _ = write!(v, " rgb(0 0 0 / {})", css_num(t.opacity));
                        }
                    }
                    (t.usage.clone(), t.name.clone(), v)
                })
                .collect(),
        ));
        sections.push((
            "Motion.".into(),
            m.motion
                .iter()
                .filter(|t| in_css(t.only))
                .map(|t| {
                    let v = match t.value {
                        Motion::Millis(ms) => format!("{ms}ms"),
                        Motion::Bezier(b) => {
                            format!("cubic-bezier({})", b.map(css_num).join(", "))
                        }
                    };
                    (t.usage.clone(), t.name.clone(), v)
                })
                .collect(),
        ));
        sections.push((
            "Unitless multiples.".into(),
            m.scale
                .iter()
                .filter(|t| in_css(t.only))
                .map(|t| (t.usage.clone(), t.name.clone(), css_num(t.value)))
                .collect(),
        ));

        let _ = writeln!(o, "\n{} {{", c.selector);
        let mut first = true;
        for (heading, lines) in sections.iter().filter(|(_, l)| !l.is_empty()) {
            if !first {
                o.push('\n');
            }
            first = false;
            let _ = writeln!(o, "  /* {} */", css_comment(heading));
            for (usage, name, value) in lines {
                if !usage.trim().is_empty() {
                    let _ = writeln!(o, "  /* {} */", css_comment(usage));
                }
                let _ = writeln!(o, "  --{name}: {value};");
            }
        }
        o.push_str("}\n");

        // The light theme's colours, where they differ from the dark one's.
        if c.appearance.is_some() && dark != light {
            let differ: Vec<&&ColorToken> = colors
                .iter()
                .filter(|t| css_color(t, dark) != css_color(t, light))
                .collect();
            if !differ.is_empty() {
                let _ = writeln!(
                    o,
                    "\n@media (prefers-color-scheme: light) {{\n  {} {{\n    /* Colours: {} (light mode), where they differ. */",
                    c.selector,
                    css_comment(&self.themes[light].name)
                );
                for t in differ {
                    let _ = writeln!(o, "    --{}: {};", t.name, css_color(t, light));
                }
                o.push_str("  }\n}\n");
            }
        }
        o
    }

    /// Prints every pair with its ratio. Returns whether any failed, and
    /// how many are pending.
    fn check_contrast(&self, w: &mut impl std::io::Write) -> (bool, usize) {
        let mut failed = false;
        let mut pending_count = 0;
        for pair in &self.contrast {
            let themes: Vec<usize> = match pair.theme {
                Some(t) => vec![t],
                None => (0..self.themes.len()).collect(),
            };
            for t in themes {
                // One number for the printout and both comparisons.
                let ratio = shown_ratio(self.ratio(pair, t), pair.min);
                let passes = ratio >= pair.min;
                let status = match (passes, &pair.pending) {
                    (true, _) => "pass",
                    (false, Some(_)) => "PENDING",
                    (false, None) => "FAIL",
                };
                let alpha = |i: usize, a: f64| a * self.colors[i].alphas[t];
                let mut fg = self.colors[pair.fg].name.clone();
                let fg_alpha = alpha(pair.fg, pair.fg_alpha);
                if fg_alpha < 1.0 {
                    let _ = write!(fg, "/{}", (fg_alpha * 100.0).round());
                }
                let mut bg = self.colors[pair.bg].name.clone();
                let bg_alpha = alpha(pair.bg, pair.bg_alpha);
                if bg_alpha < 1.0 {
                    let _ = write!(bg, "/{}", (bg_alpha * 100.0).round());
                    for (c, a) in pair.bg_over.iter().rev() {
                        let _ = write!(bg, " over {}", self.colors[*c].name);
                        let a = alpha(*c, *a);
                        if a < 1.0 {
                            let _ = write!(bg, "/{}", (a * 100.0).round());
                        }
                    }
                }
                let theme = if self.themes.len() > 1 {
                    format!(" [{}]", self.themes[t].id)
                } else {
                    String::new()
                };
                let _ = writeln!(
                    w,
                    "  {status:<7} {ratio:5.2}:1 (min {})  {fg} on {bg}{theme}: {}",
                    pair.min, pair.usage
                );
                if let Some(p) = &pair.pending {
                    let worse = ratio < p.was;
                    if passes {
                        let _ = writeln!(
                            w,
                            "            Passes now: remove \"pending\", \"ticket\" and \"was\" from this pair."
                        );
                        failed = true;
                    } else if worse {
                        let _ = writeln!(
                            w,
                            "            Worse than the {}:1 recorded when it was marked pending ({}).",
                            p.was, p.ticket
                        );
                        failed = true;
                    } else {
                        pending_count += 1;
                        let _ = writeln!(w, "            {}: {}", p.ticket, p.note);
                    }
                }
                if !passes && pair.pending.is_none() {
                    failed = true;
                }
            }
        }
        (failed, pending_count)
    }

    fn ratio(&self, pair: &Pair, theme: usize) -> f64 {
        let value = |i: usize| self.colors[i].values[theme].map(f64::from);
        // A colour's own alpha multiplies the pair's.
        let alpha = |i: usize, a: f64| a * self.colors[i].alphas[theme];
        let mut bg = value(pair.bg);
        if let Some((&(first, _), rest)) = pair.bg_over.split_first() {
            let under = rest.iter().fold(value(first), |under, &(c, a)| {
                blend(value(c), under, alpha(c, a))
            });
            bg = blend(bg, under, alpha(pair.bg, pair.bg_alpha));
        }
        let fg = blend(value(pair.fg), bg, alpha(pair.fg, pair.fg_alpha));
        contrast(fg, bg)
    }
}

const WEIGHTS: [(u16, &str); 9] = [
    (100, "Thin"),
    (200, "ExtraLight"),
    (300, "Light"),
    (400, "Regular"),
    (500, "Medium"),
    (600, "SemiBold"),
    (700, "Bold"),
    (800, "ExtraBold"),
    (900, "Black"),
];

const TEXT_STYLES: [&str; 11] = [
    "largeTitle",
    "title",
    "title2",
    "title3",
    "headline",
    "subheadline",
    "body",
    "callout",
    "footnote",
    "caption",
    "caption2",
];

/// `bgOver`: one colour, or a stack of layers bottom first, each a colour
/// name or `{"color", "alpha"}`. The bottom layer is opaque.
fn bg_layers(
    v: Option<&Value>,
    color_index: &dyn Fn(&str) -> Option<usize>,
    at: &str,
    errors: &mut Vec<String>,
) -> Vec<(usize, f64)> {
    let items: Vec<&Value> = match v {
        None => return Vec::new(),
        Some(Value::Arr(a)) => a.iter().collect(),
        Some(one) => vec![one],
    };
    let before = errors.len();
    let mut layers = Vec::new();
    for (n, item) in items.iter().enumerate() {
        let (name, alpha) = match item {
            Value::Str(s) => (s.as_str(), Some(1.0)),
            Value::Obj(_) => (
                item.get("color").and_then(Value::str).unwrap_or(""),
                item.get("alpha").and_then(Value::num),
            ),
            _ => ("", None),
        };
        let Some(c) = color_index(name) else {
            errors.push(format!("{at}: unknown colour \"{name}\" in bgOver"));
            continue;
        };
        match alpha {
            Some(a) if a > 0.0 && a <= 1.0 && (n > 0 || a == 1.0) => layers.push((c, a)),
            _ => errors.push(format!(
                "{at}: bgOver's layers need an alpha above 0, up to 1, and the bottom one opaque"
            )),
        }
    }
    if layers.is_empty() && errors.len() == before {
        errors.push(format!("{at}: bgOver is empty"));
    }
    layers
}

/// The modules the Rust output writes besides one per theme.
const RUST_MODULES: [&str; 7] = [
    "type_scale",
    "spacing",
    "radius",
    "size",
    "shadow",
    "motion",
    "scale",
];

/// Rust's strict, reserved and edition-2024 keywords.
const RUST_KEYWORDS: [&str; 52] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "gen", "macro",
    "override", "priv", "try", "typeof", "unsized", "virtual", "yield",
];

/// Swift's keywords and the names a declaration can't take unescaped.
const SWIFT_KEYWORDS: [&str; 60] = [
    "associatedtype",
    "borrowing",
    "class",
    "consuming",
    "deinit",
    "enum",
    "extension",
    "fileprivate",
    "func",
    "import",
    "init",
    "inout",
    "internal",
    "let",
    "nonisolated",
    "open",
    "operator",
    "precedencegroup",
    "private",
    "protocol",
    "public",
    "rethrows",
    "static",
    "struct",
    "subscript",
    "typealias",
    "var",
    "break",
    "case",
    "catch",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "fallthrough",
    "for",
    "guard",
    "if",
    "in",
    "repeat",
    "return",
    "throw",
    "switch",
    "where",
    "while",
    "Any",
    "Protocol",
    "Type",
    "as",
    "await",
    "false",
    "is",
    "nil",
    "self",
    "Self",
    "super",
    "throws",
    "true",
    "try",
];

/// A token's optional `"swift"` name, which replaces the camel-case one.
fn swift_override(t: &Value, at: &str, errors: &mut Vec<String>) -> Option<String> {
    let s = t.get("swift")?.str().unwrap_or("").to_string();
    if !good_ident(&s) || SWIFT_KEYWORDS.contains(&s.as_str()) {
        errors.push(format!(
            "{at}: swift name \"{s}\" isn't a usable identifier"
        ));
    }
    Some(s)
}

/// The Swift identifier for a token: its override, or its name in camel
/// case.
fn swift_id(name: &str, swift: &Option<String>) -> String {
    swift.clone().unwrap_or_else(|| camel(name))
}

/// Records `id` in one generated scope: an error if it's taken there, or
/// (when it was derived, not given) if it's a reserved word.
fn claim(
    scope: &str,
    id: String,
    derived: bool,
    reserved: &[&str],
    taken: &mut BTreeSet<(String, String)>,
    errors: &mut Vec<String>,
) {
    if derived && reserved.contains(&id.as_str()) {
        errors.push(format!(
            "{scope}: \"{id}\" is a reserved word; rename the token or give it an explicit name"
        ));
    }
    if !taken.insert((scope.to_string(), id.clone())) {
        errors.push(format!(
            "{scope}: two tokens generate \"{id}\"; rename one or give it an explicit name"
        ));
    }
}

impl Tokens {
    /// The identifiers each output generates are distinct in their scope
    /// and aren't reserved words, so the generated code always compiles.
    fn check_identifiers(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let mut taken = BTreeSet::new();
        let e = &mut errors;
        let t = &mut taken;
        if self.rust.is_some() {
            // The theme modules share the top level with the fixed ones.
            for fixed in RUST_MODULES {
                claim("rust top level", fixed.to_string(), false, &[], t, e);
            }
            for theme in &self.themes {
                claim(
                    "rust top level",
                    theme.id.clone(),
                    true,
                    &RUST_KEYWORDS,
                    t,
                    e,
                );
            }
            // A colour with an alpha also makes `<NAME>_ALPHA`.
            for c in self.colors.iter().filter(|c| in_rust(c.only)) {
                let id = screaming(&c.name);
                if c.has_alpha {
                    claim("rust colours", format!("{id}_ALPHA"), true, &[], t, e);
                }
                claim("rust colours", id, true, &[], t, e);
            }
            for g in self.more.groups.iter().filter(|g| in_rust(g.only)) {
                let scope = format!("rust type_scale::{}", g.name.replace('-', "_"));
                claim(
                    "rust type_scale",
                    g.name.replace('-', "_"),
                    true,
                    &RUST_KEYWORDS,
                    t,
                    e,
                );
                for st in &g.styles {
                    let id = screaming(&st.name);
                    if st.weight.is_some() {
                        claim(&scope, format!("{id}_WEIGHT"), true, &[], t, e);
                    }
                    claim(&scope, id, true, &[], t, e);
                }
            }
            for f in &self.families {
                let id = f
                    .rust
                    .clone()
                    .unwrap_or_else(|| format!("FONT_{}", screaming(&f.name)));
                claim("rust top level", id, f.rust.is_none(), &RUST_KEYWORDS, t, e);
            }
            for table in &self.tables {
                let module = table.name.replace('-', "_");
                claim("rust top level", module.clone(), true, &RUST_KEYWORDS, t, e);
                let scope = format!("rust {module}");
                claim(&scope, "Row".into(), false, &[], t, e);
                claim(&scope, "ALL".into(), false, &[], t, e);
                for row in &table.rows {
                    claim(&scope, screaming(&row.name), true, &[], t, e);
                }
                let fields = format!("rust {module}::Row");
                claim(&fields, "name".into(), false, &[], t, e);
                for c in &table.columns {
                    claim(
                        &fields,
                        c.name.replace('-', "_"),
                        true,
                        &RUST_KEYWORDS,
                        t,
                        e,
                    );
                }
            }
        }
        if self.css.is_some() {
            // One name space for every custom property.
            let mut prop = |name: String| claim("css", format!("--{name}"), true, &[], t, e);
            for c in self.colors.iter().filter(|c| in_css(c.only)) {
                prop(c.name.clone());
            }
            for f in &self.families {
                prop(format!("font-{}", f.name));
            }
            for g in self.more.groups.iter().filter(|g| in_css(g.only)) {
                for st in &g.styles {
                    prop(format!("{}-size", st.name));
                    if st.weight.is_some() {
                        prop(format!("{}-weight", st.name));
                    }
                    if st.line_height.is_some() {
                        prop(format!("{}-line-height", st.name));
                    }
                    if st.letter_spacing.is_some() {
                        prop(format!("{}-letter-spacing", st.name));
                    }
                }
            }
            let m = &self.more;
            for list in [&m.spacing, &m.radius, &m.size] {
                for x in list.iter().filter(|x| in_css(x.only)) {
                    prop(x.name.clone());
                }
            }
            for x in m.shadow.iter().filter(|x| in_css(x.only)) {
                prop(x.name.clone());
            }
            for x in m.motion.iter().filter(|x| in_css(x.only)) {
                prop(x.name.clone());
            }
            for x in m.scale.iter().filter(|x| in_css(x.only)) {
                prop(x.name.clone());
            }
        }
        if let Some(s) = &self.swift {
            let k = &SWIFT_KEYWORDS;
            // The appearance colours' helper, `dynamic(_:_:alpha:)`, shares
            // the colours' enum.
            if s.platform.is_some() {
                claim(&s.enum_name, "dynamic".into(), false, &[], t, e);
            }
            for c in self.colors.iter().filter(|c| in_swift(c.only)) {
                claim(
                    &s.enum_name,
                    swift_id(&c.name, &c.swift),
                    c.swift.is_none(),
                    k,
                    t,
                    e,
                );
            }
            for g in self
                .more
                .groups
                .iter()
                .filter(|g| g.swift.is_some() && in_swift(g.only))
            {
                let scope = format!(
                    "{}.{}",
                    s.enums.type_scale,
                    g.swift.as_deref().unwrap_or("")
                );
                claim(
                    &s.enums.type_scale,
                    g.swift.clone().unwrap_or_default(),
                    false,
                    k,
                    t,
                    e,
                );
                for st in &g.styles {
                    claim(
                        &scope,
                        swift_id(&st.name, &st.swift),
                        st.swift.is_none(),
                        k,
                        t,
                        e,
                    );
                }
            }
            let m = &self.more;
            for (scope, list) in [
                (&s.enums.spacing, &m.spacing),
                (&s.enums.radius, &m.radius),
                (&s.enums.size, &m.size),
            ] {
                for x in list.iter().filter(|x| in_swift(x.only)) {
                    claim(
                        scope,
                        swift_id(&x.name, &x.swift),
                        x.swift.is_none(),
                        k,
                        t,
                        e,
                    );
                }
            }
            for x in m.shadow.iter().filter(|x| in_swift(x.only)) {
                claim(
                    &s.enums.shadow,
                    swift_id(&x.name, &x.swift),
                    x.swift.is_none(),
                    k,
                    t,
                    e,
                );
            }
            for x in m.motion.iter().filter(|x| in_swift(x.only)) {
                claim(
                    &s.enums.motion,
                    swift_id(&x.name, &x.swift),
                    x.swift.is_none(),
                    k,
                    t,
                    e,
                );
            }
            for x in m.scale.iter().filter(|x| in_swift(x.only)) {
                claim(
                    &s.enums.scale,
                    swift_id(&x.name, &x.swift),
                    x.swift.is_none(),
                    k,
                    t,
                    e,
                );
            }
            // The enums themselves share the file's top level.
            for name in [
                &s.enum_name,
                &s.enums.type_scale,
                &s.enums.spacing,
                &s.enums.radius,
                &s.enums.size,
                &s.enums.shadow,
                &s.enums.motion,
                &s.enums.scale,
            ] {
                claim("swift top level", name.clone(), true, k, t, e);
            }
        }
        errors
    }
}

/// `"12px"` as 12. Lengths are never negative.
fn parse_px(s: &str) -> Option<f64> {
    let n: f64 = s.strip_suffix("px")?.parse().ok()?;
    (fits_f32(n) && n >= 0.0).then_some(n)
}

/// `"260ms"`, `"1.5s"` or `"cubic-bezier(0.22, 1, 0.36, 1)"`.
fn parse_motion(s: &str) -> Option<Motion> {
    if let Some(inner) = s
        .strip_prefix("cubic-bezier(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let n: Vec<f64> = inner
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .ok()?;
        let [x1, y1, x2, y2] = <[f64; 4]>::try_from(n).ok()?;
        // CSS's rule: the x values stay within 0 to 1.
        return ((0.0..=1.0).contains(&x1)
            && (0.0..=1.0).contains(&x2)
            && [y1, y2].into_iter().all(fits_f32))
        .then_some(Motion::Bezier([x1, y1, x2, y2]));
    }
    // A duration is read as an exact decimal, not a float, so "1.001s" is
    // 1001 ms with no rounding, and any real fraction of a millisecond is
    // an error however small ("0.0000001ms") or however large the value.
    let (number, shift) = match s.strip_suffix("ms") {
        Some(n) => (n, 0),
        None => (s.strip_suffix('s')?, 3),
    };
    whole_millis(number, shift).map(Motion::Millis)
}

/// `number` (digits, optionally `.` and more digits) times 10^`shift`, if
/// that's a whole number up to `u32::MAX`.
fn whole_millis(number: &str, shift: usize) -> Option<u64> {
    let (int, frac) = number.split_once('.').unwrap_or((number, ""));
    let digits = |d: &str| d.bytes().all(|c| c.is_ascii_digit());
    if int.is_empty() || !digits(int) || !digits(frac) || (number.contains('.') && frac.is_empty())
    {
        return None;
    }
    // The fraction's digits past `shift` must all be zeros.
    let (kept, rest) = frac.split_at(frac.len().min(shift));
    if rest.bytes().any(|c| c != b'0') {
        return None;
    }
    let mut ms: u64 = 0;
    for c in int.bytes().chain(kept.bytes()) {
        ms = ms.checked_mul(10)?.checked_add(u64::from(c - b'0'))?;
    }
    for _ in kept.len()..shift {
        ms = ms.checked_mul(10)?;
    }
    (ms <= u64::from(u32::MAX)).then_some(ms)
}

/// Type groups, spacing, radius, size, shadow and motion: validated as
/// strictly as the colours, every problem into `errors`.
fn read_more(
    v: &Value,
    families: &[Family],
    colors: &[ColorToken],
    seen: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) -> More {
    let mut more = More::default();
    let text = |o: &Value, key: &str| o.get(key).and_then(Value::str).unwrap_or("").to_string();

    // Type groups and their styles. Style names repeat across groups (each
    // platform has a body), so they're unique within a group only.
    let mut group_names = BTreeSet::new();
    let groups = v.get("type").and_then(|t| t.get("groups"));
    for (g, group) in groups
        .and_then(Value::arr)
        .unwrap_or(&[])
        .iter()
        .enumerate()
    {
        let name = text(group, "name");
        if !good_name(&name) || RUST_KEYWORDS.contains(&name.replace('-', "_").as_str()) {
            errors.push(format!("type.groups[{g}]: bad name \"{name}\""));
        }
        if !group_names.insert(name.clone()) {
            errors.push(format!("type.groups: \"{name}\" is listed twice"));
        }
        let usage = text(group, "usage");
        if usage.trim().is_empty() {
            errors.push(format!("type.groups: \"{name}\" has no usage note"));
        }
        let swift = group.get("swift").and_then(Value::str).map(str::to_string);
        if swift.as_deref().is_some_and(|s| !good_ident(s)) {
            errors.push(format!(
                "type.groups: \"{name}\": swift name isn't an identifier"
            ));
        }
        let only = read_only(group, &format!("type.groups: \"{name}\""), errors);
        if swift.is_some() && !in_swift(only) {
            errors.push(format!(
                "type.groups: \"{name}\": a group with a swift name is written to Swift, so it can't be \"only\" another output"
            ));
        }
        let mut styles = Vec::new();
        let mut style_names = BTreeSet::new();
        for st in group.get("styles").and_then(Value::arr).unwrap_or(&[]) {
            let sname = text(st, "name");
            let at = format!("type.groups \"{name}\": style \"{sname}\"");
            if !good_name(&sname) {
                errors.push(format!("{at}: bad name"));
            }
            if !style_names.insert(sname.clone()) {
                errors.push(format!("{at} is listed twice"));
            }
            let size = parse_px(&text(st, "fontSize")).filter(|n| *n > 0.0);
            if size.is_none() {
                errors.push(format!(
                    "{at}: fontSize must be a px value within f32's range, such as \"14px\""
                ));
            }
            let weight = match st.get("fontWeight") {
                None => None,
                Some(w) => {
                    let w = w.num().unwrap_or(0.0);
                    let found = WEIGHTS.iter().find(|(n, _)| f64::from(*n) == w);
                    if found.is_none() {
                        errors.push(format!("{at}: fontWeight must be 100, 200, … 900"));
                    }
                    found.map(|(n, _)| *n)
                }
            };
            let family = match st.get("family") {
                None => None,
                Some(f) => {
                    let f = f.str().unwrap_or("");
                    let found = families.iter().position(|x| x.name == f);
                    if found.is_none() {
                        errors.push(format!("{at}: unknown family \"{f}\""));
                    }
                    found
                }
            };
            let line_height = match st.get("lineHeight") {
                None => None,
                Some(Value::Num(n)) if fits_f32(*n) && *n > 0.0 => Some(LineHeight::Multiple(*n)),
                Some(Value::Str(s)) if parse_px(s).is_some_and(|n| n > 0.0) => {
                    parse_px(s).map(LineHeight::Px)
                }
                Some(_) => {
                    errors.push(format!(
                        "{at}: lineHeight must be a number above 0 (a multiple of fontSize, such as 1.25) or a px value (\"20px\")"
                    ));
                    None
                }
            };
            let letter_spacing = match st.get("letterSpacing") {
                None => None,
                Some(v) => {
                    let found = v.str().and_then(parse_letter_spacing);
                    if found.is_none() {
                        errors.push(format!(
                            "{at}: letterSpacing must be in em or px, such as \"-0.02em\" or \"0.5px\", or \"0\""
                        ));
                    }
                    found
                }
            };
            let relative_to = st
                .get("relativeTo")
                .map(|r| r.str().unwrap_or("").to_string());
            if let Some(r) = &relative_to {
                if !TEXT_STYLES.contains(&r.as_str()) {
                    errors.push(format!("{at}: relativeTo must be a SwiftUI text style"));
                }
            }
            if swift.is_some() {
                // A SwiftUI Font needs the face: family and weight.
                if family.is_some_and(|f| families[f].postscript.is_none()) {
                    errors.push(format!(
                        "{at}: its family needs a postscript name for Swift"
                    ));
                }
                if st.get("family").is_none() || weight.is_none() {
                    errors.push(format!(
                        "{at}: a Swift group's styles need family and fontWeight"
                    ));
                }
            }
            let usage = text(st, "usage");
            if usage.trim().is_empty() {
                errors.push(format!("{at}: no usage note"));
            }
            let swift_name = swift_override(st, &at, errors);
            styles.push(Style {
                name: sname,
                family,
                size: size.unwrap_or(0.0),
                weight,
                line_height,
                letter_spacing,
                relative_to,
                usage,
                swift: swift_name,
            });
        }
        if styles.is_empty() {
            errors.push(format!("type.groups: \"{name}\" has no styles"));
        }
        more.groups.push(TypeGroup {
            name,
            usage,
            swift,
            only,
            styles,
        });
    }

    // Every other family's tokens share one name space with the colours.
    type Read = (String, Value, String, Option<String>, Option<Only>);
    let mut tokens = |family: &str, errors: &mut Vec<String>| -> Vec<Read> {
        let mut out = Vec::new();
        let list = v.get(family).and_then(|f| f.get("tokens"));
        if v.get(family).is_some() && list.and_then(Value::arr).is_none() {
            errors.push(format!("{family}: needs a \"tokens\" list"));
        }
        for (i, t) in list.and_then(Value::arr).unwrap_or(&[]).iter().enumerate() {
            let name = text(t, "name");
            if !good_name(&name) {
                errors.push(format!("{family}.tokens[{i}]: bad name \"{name}\""));
            }
            if !seen.insert(name.clone()) {
                errors.push(format!("{family}: \"{name}\" is already a token's name"));
            }
            let usage = text(t, "usage");
            if usage.trim().is_empty() {
                errors.push(format!("{family}: \"{name}\" has no usage note"));
            }
            let swift = swift_override(t, &format!("{family}: \"{name}\""), errors);
            let only = read_only(t, &format!("{family}: \"{name}\""), errors);
            out.push((
                name,
                t.get("value").cloned().unwrap_or(Value::Null),
                usage,
                swift,
                only,
            ));
        }
        out
    };

    for (family, into) in [
        ("spacing", &mut more.spacing),
        ("radius", &mut more.radius),
        ("size", &mut more.size),
    ] {
        for (name, value, usage, swift, only) in tokens(family, errors) {
            match value.str().and_then(parse_px) {
                Some(px) => into.push(Length {
                    name,
                    px,
                    usage,
                    swift,
                    only,
                }),
                None => errors.push(format!(
                    "{family}: \"{name}\" needs a px value within f32's range, such as \"16px\""
                )),
            }
        }
    }

    for (name, value, usage, swift, only) in tokens("shadow", errors) {
        let px = |key: &str| {
            value
                .get(key)
                .and_then(Value::str)
                .and_then(parse_px_signed)
        };
        let spread = match value.get("spread") {
            None => Some(None),
            Some(s) => s.str().and_then(parse_px_signed).map(Some),
        };
        // Black at an opacity, or a colour token (with its own alpha).
        let paint = match (value.get("opacity"), value.get("color")) {
            (Some(o), None) => o.num().filter(|o| *o > 0.0 && *o <= 1.0).map(|o| (o, None)),
            (None, Some(c)) => {
                let name = c.str().unwrap_or("");
                colors
                    .iter()
                    .position(|t| t.name == name)
                    .map(|i| (1.0, Some(i)))
            }
            _ => None,
        };
        match (
            px("x"),
            px("y"),
            value.get("blur").and_then(Value::str).and_then(parse_px),
            spread,
            paint,
        ) {
            (Some(x), Some(y), Some(blur), Some(spread), Some((opacity, color))) => {
                if color.is_some_and(|c| in_rust(only) && !in_rust(colors[c].only)) {
                    errors.push(format!(
                        "shadow: \"{name}\": its colour isn't written to Rust, so the Rust output can't name it"
                    ));
                }
                more.shadow.push(ShadowToken {
                    name,
                    x,
                    y,
                    blur,
                    spread,
                    opacity,
                    color,
                    usage,
                    swift,
                    only,
                })
            }
            _ => errors.push(format!(
                "shadow: \"{name}\" needs {{\"x\", \"y\", \"blur\"}} in px (and optionally a \"spread\" in px), and either an opacity above 0, up to 1, or a \"color\" naming a colour token"
            )),
        }
    }

    for (name, value, usage, swift, only) in tokens("motion", errors) {
        match value.str().and_then(parse_motion) {
            Some(m) => more.motion.push(MotionToken {
                name,
                value: m,
                usage,
                swift,
                only,
            }),
            None => errors.push(format!(
                "motion: \"{name}\" needs a duration (\"260ms\", \"1.5s\", whole milliseconds) or \"cubic-bezier(x1, y1, x2, y2)\""
            )),
        }
    }

    for (name, value, usage, swift, only) in tokens("scale", errors) {
        match value.num() {
            Some(n) if fits_f32(n) && n > 0.0 => more.scale.push(ScaleToken {
                name,
                value: n,
                usage,
                swift,
                only,
            }),
            _ => errors.push(format!(
                "scale: \"{name}\" needs a number above 0 within f32's range, such as 1.75"
            )),
        }
    }
    more
}

/// A row's constant, laid out as rustfmt lays out a struct literal: on one
/// line while its fields, joined by ", ", take at most 18 columns
/// (`struct_lit_width`) and the line at most 100, else one field a line.
fn row_literal(name: &str, fields: &[String]) -> String {
    let joined = fields.join(", ");
    let line = format!("    pub const {name}: Row = Row {{ {joined} }};");
    if joined.len() <= 18 && line.len() <= 100 {
        return format!("{line}\n");
    }
    let mut o = format!("    pub const {name}: Row = Row {{\n");
    for f in fields {
        let _ = writeln!(o, "        {f},");
    }
    o.push_str("    };\n");
    o
}

/// A table's `ALL` constant, laid out as rustfmt (default settings, any
/// style edition) lays it out, so `cargo fmt --check` passes on the
/// generated file:
///
/// - on one line while the names, joined by ", ", take at most 60 columns
///   (rustfmt's `array_width`) and the line at most 100 (`max_width`);
/// - else, when every name is at most 10 characters
///   (`short_array_element_width_threshold`), packed: all on one line
///   inside the brackets if that line is at most 100 columns, else as
///   many a line as fit in 99;
/// - else one name a line.
///
/// A sweep of thousands of random tables through rustfmt 1.10 agrees.
fn all_rows(names: &[String]) -> String {
    let n = names.len();
    let head = format!("    pub const ALL: [Row; {n}] = [");
    let joined = names.join(", ");
    let line = format!("{head}{joined}];");
    if joined.len() <= 60 && line.len() <= 100 {
        return format!("{line}\n");
    }
    let mut o = format!("{head}\n");
    const INDENT: &str = "        ";
    if names.iter().all(|x| x.len() <= 10) {
        let packed = format!("{INDENT}{joined},");
        if packed.len() <= 100 {
            let _ = writeln!(o, "{packed}");
        } else {
            let mut line = String::new();
            for name in names {
                if !line.is_empty() && INDENT.len() + line.len() + 1 + name.len() + 1 > 99 {
                    let _ = writeln!(o, "{INDENT}{line}");
                    line.clear();
                }
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(name);
                line.push(',');
            }
            let _ = writeln!(o, "{INDENT}{line}");
        }
    } else {
        for name in names {
            let _ = writeln!(o, "{INDENT}{name},");
        }
    }
    o.push_str("    ];\n");
    o
}

/// `tables`: each one's columns, then its rows, validated as strictly as
/// the other families. Table names share the token names' space; column
/// and row names are unique within their table.
fn read_tables(
    v: &Value,
    themes: &[Theme],
    colors: &[ColorToken],
    seen: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) -> Vec<Table> {
    let text = |o: &Value, key: &str| o.get(key).and_then(Value::str).unwrap_or("").to_string();
    let mut tables = Vec::new();
    let list = match v.get("tables") {
        None => return tables,
        Some(Value::Arr(a)) => a,
        Some(_) => {
            errors.push("tables: must be a list".into());
            return tables;
        }
    };
    for (i, t) in list.iter().enumerate() {
        let name = text(t, "name");
        if !good_name(&name) {
            errors.push(format!("tables[{i}]: bad name \"{name}\""));
        }
        if !seen.insert(name.clone()) {
            errors.push(format!("tables: \"{name}\" is already a token's name"));
        }
        let at = format!("tables \"{name}\"");
        let usage = text(t, "usage");
        if usage.trim().is_empty() {
            errors.push(format!("{at}: no usage note"));
        }

        let mut columns: Vec<Column> = Vec::new();
        for c in t.get("columns").and_then(Value::arr).unwrap_or(&[]) {
            let cname = text(c, "name");
            let cat = format!("{at}: column \"{cname}\"");
            if !good_name(&cname) {
                errors.push(format!("{cat}: bad name"));
            }
            if columns.iter().any(|x| x.name == cname) {
                errors.push(format!("{cat} is listed twice"));
            }
            let kind_name = text(c, "type");
            let kind = COLUMN_KINDS
                .iter()
                .find(|(k, _)| *k == kind_name)
                .map(|(_, k)| *k);
            if kind.is_none() {
                errors.push(format!(
                    "{cat}: type must be color, time, px, number or text"
                ));
            }
            if kind == Some(ColumnKind::Color) && themes.len() != 1 {
                errors.push(format!(
                    "{cat}: a color column names one theme's colour; it needs exactly one theme"
                ));
            }
            let cusage = text(c, "usage");
            if cusage.trim().is_empty() {
                errors.push(format!("{cat}: no usage note"));
            }
            columns.push(Column {
                name: cname,
                kind: kind.unwrap_or(ColumnKind::Text),
                usage: cusage,
            });
        }
        if columns.is_empty() {
            errors.push(format!(
                "{at}: needs \"columns\", a list of {{name, type, usage}}"
            ));
        }

        let mut rows: Vec<Row> = Vec::new();
        for r in t.get("rows").and_then(Value::arr).unwrap_or(&[]) {
            let rname = text(r, "name");
            let rat = format!("{at}: row \"{rname}\"");
            if !good_name(&rname) {
                errors.push(format!("{rat}: bad name"));
            }
            if rows.iter().any(|x| x.name == rname) {
                errors.push(format!("{rat} is listed twice"));
            }
            let rusage = text(r, "usage");
            if rusage.trim().is_empty() {
                errors.push(format!("{rat}: no usage note"));
            }
            let value = r.get("value");
            match value {
                Some(Value::Obj(entries)) => {
                    for (key, _) in entries {
                        if !columns.iter().any(|c| &c.name == key) {
                            errors.push(format!("{rat}: unknown column \"{key}\""));
                        }
                    }
                }
                _ => errors.push(format!(
                    "{rat}: value must be an object of column name to value"
                )),
            }
            let mut cells = Vec::new();
            for c in &columns {
                let cell = value.and_then(|v| v.get(&c.name));
                let parsed = match (c.kind, cell) {
                    (_, None) => {
                        errors.push(format!("{rat}: no value for column \"{}\"", c.name));
                        None
                    }
                    (ColumnKind::Color, Some(x)) => {
                        let n = x.str().unwrap_or("");
                        // Tables are Rust only, so the colour must be too.
                        let found = colors
                            .iter()
                            .position(|k| k.name == n && in_rust(k.only))
                            .map(Cell::Color);
                        if let Some(Cell::Color(k)) = found {
                            // A cell is a u32 with no alpha; the app would
                            // draw it opaque.
                            if colors[k].has_alpha {
                                errors.push(format!(
                                    "{rat}: \"{}\" names \"{n}\", which has an alpha; a table's colour cell can't carry one, so name an opaque colour (and put the alpha in a number column)",
                                    c.name
                                ));
                            }
                        }
                        if found.is_none() {
                            errors.push(format!(
                                "{rat}: \"{}\" must name a colour token written to Rust; \"{n}\" isn't one",
                                c.name
                            ));
                        }
                        found
                    }
                    (ColumnKind::Time, Some(x)) => {
                        let found = x.str().and_then(parse_time).map(Cell::Minutes);
                        if found.is_none() {
                            errors.push(format!(
                                "{rat}: \"{}\" must be a time of day, \"HH:MM\" from 00:00 to 23:59",
                                c.name
                            ));
                        }
                        found
                    }
                    (ColumnKind::Px, Some(x)) => {
                        let found = x.str().and_then(parse_px).map(Cell::Num);
                        if found.is_none() {
                            errors.push(format!(
                                "{rat}: \"{}\" must be a px value within f32's range, such as \"16px\"",
                                c.name
                            ));
                        }
                        found
                    }
                    (ColumnKind::Number, Some(x)) => {
                        let found = x.num().filter(|n| fits_f32(*n)).map(Cell::Num);
                        if found.is_none() {
                            errors.push(format!(
                                "{rat}: \"{}\" must be a number within f32's range",
                                c.name
                            ));
                        }
                        found
                    }
                    (ColumnKind::Text, Some(x)) => {
                        let found = x
                            .str()
                            .filter(|s| !s.contains(['"', '\\']) && !s.contains(char::is_control))
                            .map(|s| Cell::Text(s.to_string()));
                        if found.is_none() {
                            errors.push(format!(
                                "{rat}: \"{}\" must be a string without quotes, backslashes or control characters",
                                c.name
                            ));
                        }
                        found
                    }
                };
                cells.push(parsed.unwrap_or(Cell::Num(0.0)));
            }
            rows.push(Row {
                name: rname,
                usage: rusage,
                cells,
            });
        }
        if rows.is_empty() {
            errors.push(format!(
                "{at}: needs \"rows\", a list of {{name, value, usage}}"
            ));
        }
        tables.push(Table {
            name,
            usage,
            columns,
            rows,
        });
    }
    tables
}

/// A table's number cell named `"table.row.column"`.
fn table_number(tables: &[Table], r: &str) -> Result<f64, String> {
    let mut parts = r.splitn(3, '.');
    let (Some(t), Some(row), Some(col)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("\"{r}\" isn't \"table.row.column\""));
    };
    let table = tables
        .iter()
        .find(|x| x.name == t)
        .ok_or_else(|| format!("no table \"{t}\""))?;
    let c = table
        .columns
        .iter()
        .position(|x| x.name == col)
        .ok_or_else(|| format!("table \"{t}\" has no column \"{col}\""))?;
    if table.columns[c].kind != ColumnKind::Number {
        return Err(format!("\"{r}\" isn't a number column"));
    }
    let row = table
        .rows
        .iter()
        .find(|x| x.name == row)
        .ok_or_else(|| format!("table \"{t}\" has no row \"{row}\""))?;
    match row.cells[c] {
        Cell::Num(n) => Ok(n),
        _ => Err(format!("\"{r}\" isn't a number")),
    }
}

/// A number the Rust output can write as an `f32`: finite and within its
/// range (`1e39` would be infinity).
fn fits_f32(n: f64) -> bool {
    n.is_finite() && n.abs() <= f64::from(f32::MAX)
}

/// `"05:00"` as 300: a time of day, 00:00 to 23:59.
fn parse_time(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 || !(h.bytes().chain(m.bytes())).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// An offset: like `parse_px`, but it may be negative.
fn parse_px_signed(s: &str) -> Option<f64> {
    let n: f64 = s.strip_suffix("px")?.parse().ok()?;
    fits_f32(n).then_some(n)
}

/// `"-0.02em"`, `"0.5px"` or `"0"`: a letter spacing, which may be
/// negative.
fn parse_letter_spacing(s: &str) -> Option<Spacing> {
    if s == "0" {
        return Some(Spacing::Em(0.0));
    }
    if let Some(n) = s.strip_suffix("em") {
        let n: f64 = n.parse().ok()?;
        return fits_f32(n).then_some(Spacing::Em(n));
    }
    parse_px_signed(s).map(Spacing::Px)
}

/// A CSS number: the shortest that reads back as the same `f64`, with no
/// trailing `.0` (`2`, `0.8125`, `-0.02`).
fn css_num(x: f64) -> String {
    let x = if x == 0.0 { 0.0 } else { x };
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

/// A colour in one theme: `#rrggbb`, or `rgb(r g b / a)` where it's
/// translucent.
fn css_color(c: &ColorToken, theme: usize) -> String {
    let [r, g, b] = c.values[theme];
    let a = c.alphas[theme];
    if a < 1.0 {
        format!("rgb({r} {g} {b} / {})", css_num(a))
    } else {
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// px as rem at the browsers' default 16 px root, so text follows the
/// reader's font size: 56px is `3.5rem`.
fn rem(px: f64) -> String {
    format!("{}rem", css_num(px / 16.0))
}

/// Text a CSS value or selector can hold without ending its declaration,
/// rule, string or a comment early: no braces, semicolons, backslashes,
/// comment markers or control characters, and quotes only in pairs.
fn css_safe(s: &str) -> bool {
    !s.contains(['{', '}', ';', '\\'])
        && !s.contains("/*")
        && !s.contains("*/")
        && !s.contains(char::is_control)
        && s.matches('"').count().is_multiple_of(2)
        && s.matches('\'').count().is_multiple_of(2)
}

/// A family's `css` fallback list (`ui-monospace, monospace`).
fn good_css_fallback(s: &str) -> bool {
    !s.trim().is_empty() && css_safe(s)
}

/// `outputs.css.selector` (`:root`, `.calc`).
fn good_selector(s: &str) -> bool {
    !s.is_empty() && css_safe(s) && !s.contains('@')
}

/// One line of text, safe in a `/* */` comment.
fn css_comment(text: &str) -> String {
    doc(text).replace("*/", "* /")
}

/// A Rust `f32` literal: the shortest that reads back as the same `f32`,
/// so clippy's excessive_precision never fires on generated code.
fn rs_num(x: f64) -> String {
    let x = x as f32;
    if x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

/// A Swift number literal.
fn swift_num(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{x:.0}")
    } else {
        format!("{x}")
    }
}

fn seconds(ms: u64) -> String {
    swift_num(ms as f64 / 1000.0)
}

// ------------------------------------------------------------------ colour

/// `alpha` of `fg` composited over `bg`, as the renderers do.
fn blend(fg: [f64; 3], bg: [f64; 3], alpha: f64) -> [f64; 3] {
    [0, 1, 2].map(|i| alpha * fg[i] + (1.0 - alpha) * bg[i])
}

/// WCAG 2.2 relative luminance of 0–255 channels.
fn luminance(c: [f64; 3]) -> f64 {
    let lin = |v: f64| {
        let v = v / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// The contrast minimums a pair can ask for. Each has at most two decimals
/// (a test checks), so `shown_ratio` keeps its promise.
const MINIMUMS: [f64; 3] = [3.0, 4.5, 7.0];

/// The ratio the check prints, and compares with the pair's `min` and
/// `was`: rounded to two decimals, except that one just under `min` is cut
/// instead (4.499 is 4.49, not 4.50). So it's at least `min` exactly when
/// the unrounded ratio is, as WCAG has it (4.499:1 misses 4.5), and the
/// printout never shows a failing pair at its minimum.
fn shown_ratio(ratio: f64, min: f64) -> f64 {
    let rounded = two_places(ratio);
    if ratio < min && rounded >= min {
        (ratio * 100.0).floor() / 100.0
    } else {
        rounded
    }
}

fn two_places(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

// ------------------------------------------------------------------ names

fn screaming(name: &str) -> String {
    name.replace('-', "_").to_ascii_uppercase()
}

fn camel(name: &str) -> String {
    let mut parts = name.split('-');
    let mut out = parts.next().unwrap_or("").to_string();
    for p in parts {
        let mut chars = p.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.extend(chars);
        }
    }
    out
}

/// One line of text, safe in a `///` or `//` comment.
fn doc(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------ JSON

/// A JSON value. Objects keep their keys' order.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    fn num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    fn arr(&self) -> Option<&[Value]> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }
}

/// Parses a JSON document (RFC 8259). Errors give the line and column.
fn parse(text: &str) -> Result<Value, String> {
    // RFC 8259 forbids a byte order mark; some editors add one.
    if text.starts_with('\u{feff}') {
        return Err("it starts with a byte order mark (BOM): save it as UTF-8 without one".into());
    }
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
    };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.error("text after the end of the document"));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        let before = &self.s[..self.i.min(self.s.len())];
        let line = before.iter().filter(|&&c| c == b'\n').count() + 1;
        let col = before.iter().rev().take_while(|&&c| c != b'\n').count() + 1;
        format!("{what} at line {line}, column {col}")
    }

    fn ws(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected '{}'", c as char)))
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            Err(self.error("unexpected character"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 64 {
            return Err(self.error("nested too deeply"));
        }
        match self.s.get(self.i) {
            None => Err(self.error("unexpected end")),
            Some(b'{') => {
                self.i += 1;
                let mut entries = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Value::Obj(entries));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    if entries.iter().any(|(k, _)| *k == key) {
                        return Err(self.error(&format!("duplicate key \"{key}\"")));
                    }
                    self.ws();
                    self.eat(b':')?;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    entries.push((key, v));
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Obj(entries));
                        }
                        _ => return Err(self.error("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return Err(self.error("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => self.string().map(Value::Str),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(c) if *c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.error("unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let from = p.i;
            while p.s.get(p.i).is_some_and(u8::is_ascii_digit) {
                p.i += 1;
            }
            p.i > from
        };
        if self.s.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        if self.s.get(self.i) == Some(&b'0') {
            self.i += 1;
        } else if !digits(self) {
            return Err(self.error("bad number"));
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.error("bad number"));
            }
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.error("bad number"));
            }
        }
        let text =
            std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.error("bad number"))?;
        text.parse()
            .map(Value::Num)
            .map_err(|_| self.error("bad number"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self
            .s
            .get(self.i..self.i + 4)
            // Four hex digits exactly: from_str_radix alone takes a sign.
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .ok_or_else(|| self.error("bad \\u escape"))?;
        self.i += 4;
        Ok(h)
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = Vec::new();
        loop {
            match self.s.get(self.i) {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return String::from_utf8(out).map_err(|_| self.error("invalid UTF-8"));
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = *self.s.get(self.i).ok_or_else(|| self.error("bad escape"))?;
                    self.i += 1;
                    let ch = match c {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                if self.s.get(self.i..self.i + 2) != Some(b"\\u") {
                                    return Err(self.error("lone surrogate"));
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.error("lone surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or_else(|| self.error("bad \\u escape"))?
                        }
                        _ => return Err(self.error("bad escape")),
                    };
                    let mut buf = [0; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                Some(c) if *c < 0x20 => return Err(self.error("control character in a string")),
                Some(c) => {
                    out.push(*c);
                    self.i += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r##"{
      "name": "Test",
      "outputs": { "rust": { "path": "src/tokens.rs" } },
      "color": {
        "themes": [{ "id": "dark", "name": "Dark" }],
        "tokens": [
          { "name": "ground", "value": { "dark": "#000000" }, "usage": "Background." },
          { "name": "text", "value": { "dark": "#ffffff" }, "usage": "Text." },
          { "name": "dim", "value": { "dark": "#333333" }, "usage": "Edges." }
        ]
      },
      "contrast": [
        { "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }
      ]
    }"##;

    fn tokens(json: &str) -> Result<Tokens, Vec<String>> {
        Tokens::read(&parse(json).expect("valid JSON"))
    }

    #[test]
    fn json_parses_what_tokens_use() {
        let v = parse(r#"{"a": [1, -2.5e1, true, null, "x\"é😀"], "b": {}}"#).unwrap();
        assert_eq!(
            v.get("a").unwrap().arr().unwrap(),
            &[
                Value::Num(1.0),
                Value::Num(-25.0),
                Value::Bool(true),
                Value::Null,
                Value::Str("x\"é😀".into())
            ]
        );
        assert_eq!(v.get("b"), Some(&Value::Obj(vec![])));
        for bad in [
            r#"{"a": 1,}"#,
            r#"{"a" 1}"#,
            "[01]",
            r#"{"a":1,"a":2}"#,
            "\"\u{1}\"",
            "[] x",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse("{\n  \"a\": tru\n}").unwrap_err().contains("line 2"));
    }

    #[test]
    fn wcag_ratios() {
        assert_eq!(two_places(contrast([0.0; 3], [255.0; 3])), 21.0);
        // Nitrate's outline on its ground, as the design canvas has it.
        let c = |h| parse_hex(h).unwrap().map(f64::from);
        assert_eq!(two_places(contrast(c("#3a4a68"), c("#0b111d"))), 2.12);
        // Translucent: white at 50% over black is mid grey.
        assert_eq!(blend([255.0; 3], [0.0; 3], 0.5), [127.5; 3]);
    }

    #[test]
    fn minimal_tokens_build() {
        let t = tokens(MINIMAL).unwrap();
        let out = t.outputs();
        assert_eq!(out.len(), 1);
        assert!(out[0].1.starts_with(&format!("//! {HEADER}\n")));
        assert!(out[0].1.contains("pub const TEXT: u32 = 0xFFFFFF;"));
        let (failed, pending) = t.check_contrast(&mut Vec::new());
        assert!(!failed);
        assert_eq!(pending, 0);
    }

    #[test]
    fn bad_tokens_are_errors_not_crashes() {
        let errors = tokens(
            r##"{"name": "T", "color": {"themes": [{"id": "dark"}, {"id": "light"}],
               "tokens": [{"name": "Bad Name", "value": {"dark": "#FFF"}},
                          {"name": "a", "value": {"dark": "#000000", "dim": "#000000"}, "usage": "x"}]},
               "contrast": [{"fg": "a", "bg": "nope", "min": 2},
                            {"fg": "a", "bg": "a", "min": 3, "usage": "x", "pending": "p", "ticket": "X-1"}],
               "outputs": {"swift": {"path": "../x.swift"}}}"##,
        )
        .unwrap_err();
        let all = errors.join("\n");
        for want in [
            "bad name \"Bad Name\"",
            "no usage note",
            "lowercase #rrggbb value for theme \"dark\"",
            "for theme \"light\"",
            "unknown theme \"dim\"",
            "unknown colour \"nope\"",
            "min must be 3, 4.5 or 7",
            "needs the ticket",
            "needs \"was\"",
            "names its theme",
            "inside the repo",
        ] {
            assert!(all.contains(want), "missing {want:?} in:\n{all}");
        }
        assert!(tokens("[]").is_err());
    }

    fn with_pair(pair: &str) -> Tokens {
        tokens(&MINIMAL.replace(
            r#"{ "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }"#,
            pair,
        ))
        .unwrap()
    }

    #[test]
    fn pending_pairs_fail_when_worse_or_fixed() {
        // dim on ground: 1.66:1.
        let pending = |was: &str| {
            with_pair(&format!(
                r#"{{ "fg": "dim", "bg": "ground", "min": 3, "usage": "Edge.", "pending": "Fix.", "ticket": "ALI-1", "was": {was} }}"#
            ))
            .check_contrast(&mut Vec::new())
        };
        assert_eq!(pending("1.66"), (false, 1), "as recorded");
        assert_eq!(pending("1.5"), (false, 1), "better, still failing");
        assert!(pending("1.67").0, "worse than recorded");
        let fixed = with_pair(
            r#"{ "fg": "text", "bg": "ground", "min": 3, "usage": "Edge.", "pending": "Fix.", "ticket": "ALI-1", "was": 1.2 }"#,
        );
        assert!(fixed.check_contrast(&mut Vec::new()).0, "passes now");
        let failing = with_pair(r#"{ "fg": "dim", "bg": "ground", "min": 3, "usage": "Edge." }"#);
        assert!(failing.check_contrast(&mut Vec::new()).0);
    }

    #[test]
    fn translucent_pairs_composite_first() {
        // Text at 50% over ground is mid grey: 5.28:1 on black.
        let t = with_pair(
            r#"{ "fg": "text", "fgAlpha": 0.5, "bg": "ground", "min": 4.5, "usage": "Dim text." }"#,
        );
        assert_eq!(two_places(t.ratio(&t.contrast[0], 0)), 5.28);
        // Text on a 10% text fill over the ground.
        let t = with_pair(
            r#"{ "fg": "text", "bg": "text", "bgAlpha": 0.1, "bgOver": "ground", "min": 4.5, "usage": "Chip." }"#,
        );
        let fill = blend([255.0; 3], [0.0; 3], 0.1);
        assert_eq!(t.ratio(&t.contrast[0], 0), contrast([255.0; 3], fill));
        assert!(
            tokens(&MINIMAL.replace(
                r#""min": 4.5, "usage": "Body.""#,
                r#""bgAlpha": 0.5, "min": 4.5, "usage": "Body.""#
            ))
            .is_err(),
            "bgAlpha needs bgOver"
        );
    }

    #[test]
    fn names() {
        assert_eq!(screaming("text-secondary"), "TEXT_SECONDARY");
        assert_eq!(camel("on-accent"), "onAccent");
        assert!(good_name("accent-hover") && !good_name("Accent") && !good_name("a--b"));
        assert!(good_ticket("ALI-2120") && !good_ticket("ALI-") && !good_ticket("ali-1"));
    }

    /// MINIMAL plus one of each other family, and a Swift output.
    fn full() -> Result<Tokens, Vec<String>> {
        tokens(&MINIMAL.replace(
            r#""outputs": { "rust": { "path": "src/tokens.rs" } },"#,
            r#""outputs": { "rust": { "path": "src/tokens.rs" }, "swift": { "path": "T.swift" } },
                "type": {
                  "families": [{ "name": "ui", "value": "Inter", "postscript": "Inter", "usage": "UI." }],
                  "groups": [
                    { "name": "phone", "swift": "Phone", "usage": "iPhone.", "styles": [
                      { "name": "body", "family": "ui", "fontSize": "16px", "fontWeight": 600, "relativeTo": "body", "usage": "Body." }
                    ] },
                    { "name": "linux", "usage": "Linux.", "styles": [
                      { "name": "text-13", "fontSize": "13px", "usage": "Small." }
                    ] }
                  ]
                },
                "spacing": { "tokens": [{ "name": "page-x", "value": "64px", "usage": "Margin." }] },
                "radius": { "tokens": [{ "name": "radius-card", "value": "8.5px", "usage": "Cards." }] },
                "size": { "tokens": [{ "name": "button-height", "value": "44px", "usage": "Buttons." }] },
                "shadow": { "tokens": [{ "name": "shadow-sheet", "value": { "x": "0px", "y": "-2px", "blur": "64px", "opacity": 0.5 }, "usage": "Sheet." }] },
                "motion": { "tokens": [
                  { "name": "glide", "value": "0.26s", "usage": "Glide." },
                  { "name": "ease-out-quint", "value": "cubic-bezier(0.22, 1, 0.36, 1)", "usage": "Slide." }
                ] },
                "#,
        ))
    }

    #[test]
    fn every_family_reaches_rust_and_swift() {
        let t = full().unwrap();
        let out = t.outputs();
        let (rust, swift) = (&out[0].1, &out[1].1);
        for want in [
            "pub mod type_scale {",
            "    pub mod phone {",
            "        pub const BODY: f32 = 16.0;",
            "        pub const BODY_WEIGHT: f32 = 600.0;",
            "        pub const TEXT_13: f32 = 13.0;",
            "    pub const PAGE_X: f32 = 64.0;",
            "    pub const RADIUS_CARD: f32 = 8.5;",
            "    pub const BUTTON_HEIGHT: f32 = 44.0;",
            "        y: -2.0,",
            "    pub const GLIDE: Duration = Duration::from_millis(260);",
            "    pub const EASE_OUT_QUINT: [f32; 4] = [0.22, 1.0, 0.36, 1.0];",
        ] {
            assert!(rust.contains(want), "missing {want:?} in:\n{rust}");
        }
        for want in [
            "enum TypeScale {",
            "    enum Phone {",
            "static let body = Font.custom(\"Inter-SemiBold\", size: 16, relativeTo: .body)",
            "enum Spacing {",
            "    static let pageX: CGFloat = 64",
            "    static let radiusCard: CGFloat = 8.5",
            "enum Sizes {",
            "static let shadowSheet = ShadowToken(x: 0, y: -2, radius: 64, opacity: 0.5)",
            "    static let glide: Double = 0.26",
            ".timingCurve(0.22, 1, 0.36, 1, duration: duration)",
        ] {
            assert!(swift.contains(want), "missing {want:?} in:\n{swift}");
        }
        // The Linux-only group isn't written for Swift.
        assert!(!swift.contains("text13"));
    }

    #[test]
    fn other_families_are_validated() {
        // Names are unique across families: a spacing token can't be named
        // like a colour.
        let errors = tokens(&MINIMAL.replace(
            r#""contrast""#,
            r#""spacing": { "tokens": [{ "name": "ground", "value": "4px", "usage": "Clash." }] }, "contrast""#,
        ))
        .unwrap_err()
        .join("\n");
        assert!(
            errors.contains("\"ground\" is already a token's name"),
            "{errors}"
        );
        let one = |family: &str, token: &str| {
            let t = MINIMAL.replace(
                r#""contrast""#,
                &format!(r#""{family}": {{ "tokens": [{token}] }}, "contrast""#),
            );
            tokens(&t).unwrap_err().join("\n")
        };
        assert!(one(
            "spacing",
            r#"{ "name": "gap", "value": "4", "usage": "x" }"#
        )
        .contains("needs a px value"));
        assert!(one(
            "radius",
            r#"{ "name": "r", "value": "-4px", "usage": "x" }"#
        )
        .contains("needs a px value"));
        assert!(one("size", r#"{ "name": "s", "value": "4px" }"#).contains("no usage note"));
        assert!(one(
            "motion",
            r#"{ "name": "m", "value": "0.2505s", "usage": "x" }"#
        )
        .contains("whole milliseconds"));
        assert!(one(
            "motion",
            r#"{ "name": "m", "value": "cubic-bezier(2, 0, 1, 1)", "usage": "x" }"#
        )
        .contains("cubic-bezier"));
        assert!(one("shadow", r#"{ "name": "s", "value": { "x": "0px", "y": "1px", "blur": "2px", "opacity": 2 }, "usage": "x" }"#).contains("opacity above 0"));
        // Type styles.
        let style = |st: &str| {
            let t = MINIMAL.replace(
                r#""contrast""#,
                &format!(
                    r#""type": {{ "families": [{{ "name": "ui", "value": "Inter", "usage": "UI." }}],
                       "groups": [{{ "name": "mac", "swift": "Mac", "usage": "Mac.", "styles": [{st}] }}] }}, "contrast""#
                ),
            );
            tokens(&t).unwrap_err().join("\n")
        };
        assert!(style(
            r#"{ "name": "a", "family": "ui", "fontSize": "14", "fontWeight": 400, "usage": "x" }"#
        )
        .contains("fontSize must be a px value"));
        assert!(style(r#"{ "name": "a", "family": "ui", "fontSize": "14px", "fontWeight": 450, "usage": "x" }"#).contains("fontWeight must be"));
        assert!(style(r#"{ "name": "a", "family": "ui", "fontSize": "14px", "fontWeight": 400, "relativeTo": "huge", "usage": "x" }"#).contains("relativeTo"));
        assert!(style(r#"{ "name": "a", "family": "serif", "fontSize": "14px", "fontWeight": 400, "usage": "x" }"#).contains("unknown family"));
        assert!(style(r#"{ "name": "a", "family": "ui", "fontSize": "14px", "fontWeight": 400, "usage": "x" }"#).contains("postscript name"));
        assert!(
            style(r#"{ "name": "a", "fontSize": "14px", "usage": "x" }"#)
                .contains("need family and fontWeight")
        );
    }

    #[test]
    fn lengths_and_motion_parse() {
        assert_eq!(parse_px("12px"), Some(12.0));
        assert_eq!(parse_px("12"), None);
        assert_eq!(parse_px("-1px"), None);
        assert_eq!(parse_px_signed("-1px"), Some(-1.0));
        assert_eq!(parse_motion("260ms"), Some(Motion::Millis(260)));
        assert_eq!(parse_motion("1.5s"), Some(Motion::Millis(1500)));
        assert_eq!(parse_motion("1.5ms"), None);
        assert_eq!(
            parse_motion("cubic-bezier(0.4, 0, 0.2, 1)"),
            Some(Motion::Bezier([0.4, 0.0, 0.2, 1.0]))
        );
        assert_eq!(parse_motion("cubic-bezier(0.4, 0, 0.2)"), None);
        assert_eq!(rs_num(44.0), "44.0");
        assert_eq!(swift_num(0.26), "0.26");
        assert_eq!(seconds(150), "0.15");
    }

    #[test]
    fn generated_identifiers_are_distinct_and_not_reserved() {
        let errs = |json: String| tokens(&json).unwrap_err().join("\n");
        let with =
            |extra: &str| MINIMAL.replace(r#""contrast""#, &format!(r#"{extra}, "contrast""#));
        // Rust: a style's weight constant against another style's name.
        let e = errs(with(
            r#""type": { "groups": [{ "name": "mac", "usage": "Mac.", "styles": [
                { "name": "body", "fontSize": "14px", "fontWeight": 400, "usage": "x" },
                { "name": "body-weight", "fontSize": "14px", "usage": "x" }
            ] }] }"#,
        ));
        assert!(e.contains("two tokens generate \"BODY_WEIGHT\""), "{e}");
        // Rust: a theme id that's a keyword.
        let e = errs(
            MINIMAL
                .replace(r#""id": "dark""#, r#""id": "type""#)
                .replace(r#""dark":"#, r#""type":"#),
        );
        assert!(e.contains("\"type\" is a reserved word"), "{e}");
        // Swift: names that meet after camel case, and a reserved word.
        let swift = MINIMAL.replace(
            r#"{ "rust": { "path": "src/tokens.rs" } }"#,
            r#"{ "swift": { "path": "T.swift" } }"#,
        );
        let e = errs(swift.replace(
            r#""contrast""#,
            r#""spacing": { "tokens": [
                { "name": "gap-1", "value": "4px", "usage": "x" },
                { "name": "gap1", "value": "8px", "usage": "x" }
            ] }, "contrast""#,
        ));
        assert!(e.contains("Spacing: two tokens generate \"gap1\""), "{e}");
        let reserved = swift.replace(r#""name": "dim""#, r#""name": "default""#);
        assert!(errs(reserved.clone()).contains("\"default\" is a reserved word"));
        // An explicit name is the way out; a reserved one isn't accepted.
        let renamed = reserved.replace(
            r#""name": "default","#,
            r#""name": "default", "swift": "fallback","#,
        );
        assert!(tokens(&renamed).is_ok());
        let e = errs(reserved.replace(
            r#""name": "default","#,
            r#""name": "default", "swift": "init","#,
        ));
        assert!(e.contains("isn't a usable identifier"), "{e}");
    }

    #[test]
    fn a_pending_pair_records_a_real_ratio() {
        let e = tokens(&MINIMAL.replace(
            r#"{ "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }"#,
            r#"{ "fg": "dim", "bg": "ground", "min": 3, "usage": "x", "pending": "p", "ticket": "ALI-1", "was": 0 }"#,
        ))
        .unwrap_err()
        .join("\n");
        assert!(e.contains("needs \"was\""), "{e}");
    }

    #[test]
    fn reserved_words_and_rust_scopes() {
        let errs = |json: &str| tokens(json).map(|_| ()).unwrap_err().join("\n");
        let group = |name: &str| {
            MINIMAL.replace(
                r#""contrast""#,
                &format!(
                    r#""type": {{ "groups": [{{ "name": "{name}", "usage": "x", "styles": [
                        {{ "name": "body", "fontSize": "14px", "usage": "x" }} ] }}] }}, "contrast""#
                ),
            )
        };
        for word in ["final", "try"] {
            assert!(errs(&group(word)).contains("bad name"), "{word}");
        }
        // A theme and a type group may share a name: they're in different
        // Rust scopes.
        assert!(tokens(&group("dark")).is_ok());
        // A theme can't take a fixed module's name.
        let size = MINIMAL
            .replace(r#""id": "dark""#, r#""id": "size""#)
            .replace(r#""dark":"#, r#""size":"#);
        assert!(errs(&size).contains("rust top level: two tokens generate \"size\""));
        // Swift-reserved colour names.
        let swift = MINIMAL.replace(
            r#"{ "rust": { "path": "src/tokens.rs" } }"#,
            r#"{ "swift": { "path": "T.swift" } }"#,
        );
        for word in ["try", "super"] {
            let json = swift.replace(r#""name": "dim""#, &format!(r#""name": "{word}""#));
            assert!(
                errs(&json).contains(&format!("\"{word}\" is a reserved word")),
                "{word}"
            );
        }
    }

    #[test]
    fn bg_over_takes_a_stack_of_layers() {
        // Text at 50% over (text at 50% over ground): the same as one
        // layer of 75% text.
        let stacked = with_pair(
            r#"{ "fg": "text", "bg": "text", "bgAlpha": 0.5, "bgOver": ["ground", { "color": "text", "alpha": 0.5 }], "min": 3, "usage": "x" }"#,
        );
        let one = with_pair(
            r#"{ "fg": "text", "bg": "text", "bgAlpha": 0.75, "bgOver": "ground", "min": 3, "usage": "x" }"#,
        );
        assert!(
            (stacked.ratio(&stacked.contrast[0], 0) - one.ratio(&one.contrast[0], 0)).abs() < 1e-9
        );
        let mut out = Vec::new();
        stacked.check_contrast(&mut out);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("text on text/50 over text/50 over ground"));
        let bad = |over: &str| {
            tokens(&MINIMAL.replace(
                r#"{ "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }"#,
                &format!(r#"{{ "fg": "text", "bg": "text", "bgAlpha": 0.5, "bgOver": {over}, "min": 3, "usage": "x" }}"#),
            ))
            .map(|_| ())
            .unwrap_err()
            .join("\n")
        };
        assert!(bad(r#"[{ "color": "text", "alpha": 0.5 }]"#).contains("bottom one opaque"));
        assert!(bad(r#"["nope"]"#).contains("unknown colour \"nope\""));
        assert!(bad("[]").contains("bgOver is empty"));
    }

    /// Two themes, written for Rust and as UIKit/AppKit colours for Swift.
    const TWO_THEMES: &str = r##"{
      "name": "Test",
      "outputs": {
        "rust": { "path": "src/tokens.rs" },
        "swift": { "path": "P.swift", "enum": "Palette", "colorType": "PlatformColor",
                   "appearance": { "dark": "night", "light": "day" } }
      },
      "color": {
        "themes": [{ "id": "night", "name": "Night" }, { "id": "day", "name": "Day" }],
        "tokens": [
          { "name": "ground", "value": { "night": "#000000", "day": "#ffffff" }, "usage": "Ground." },
          { "name": "text", "value": { "night": "#ffffff", "day": "#000000" }, "usage": "Text." },
          { "name": "accent", "value": { "night": "#4ea1ff", "day": "#4ea1ff" }, "usage": "Fixed." },
          { "name": "dock", "value": { "night": "#202020", "day": "#f0f0f0" }, "alpha": 0.95, "only": "swift", "usage": "Dock." },
          { "name": "selection", "value": { "night": "#4ea1ff", "day": "#1a73d9" }, "alpha": { "night": 0.3, "day": 0.2 }, "only": "rust", "usage": "Selection." },
          { "name": "scrollbar", "value": { "night": "#ffffff", "day": "#000000" }, "alpha": 0.2196078431372549, "only": "rust", "usage": "Thumb." }
        ]
      },
      "scale": { "tokens": [
        { "name": "heading-linux", "value": 1.75, "only": "rust", "usage": "Linux." },
        { "name": "heading-apple", "value": 1.74, "only": "swift", "swift": "heading", "usage": "Apple." }
      ] },
      "contrast": [
        { "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." },
        { "fg": "text", "bg": "selection", "bgOver": "ground", "min": 4.5, "usage": "Selected text." }
      ]
    }"##;

    #[test]
    fn two_themes_reach_swift_as_appearance_colours() {
        let t = tokens(TWO_THEMES).unwrap();
        let out = t.outputs();
        let (rust, swift) = (&out[0].1, &out[1].1);
        for want in [
            "pub mod night {",
            "pub mod day {",
            "    pub const SELECTION: u32 = 0x1A73D9;",
            "    pub const SELECTION_ALPHA: f32 = 0.2;",
            // An f32 written no longer than it needs (56 / 255 here).
            "    pub const SCROLLBAR_ALPHA: f32 = 0.21960784;",
            "pub mod scale {",
            "    pub const HEADING_LINUX: f32 = 1.75;",
        ] {
            assert!(rust.contains(want), "missing {want:?} in:\n{rust}");
        }
        // Swift-only tokens stay out of Rust, and Rust-only ones out of Swift.
        assert!(!rust.contains("DOCK") && !rust.contains("HEADING_APPLE"));
        for want in [
            ") -> PlatformColor {",
            "traits.userInterfaceStyle == .light",
            "appearance.bestMatch(from: [.darkAqua, .aqua]) == .aqua",
            "static let ground = dynamic(0x000000, 0xFFFFFF)",
            "static let accent = PlatformColor(hex: 0x4EA1FF)",
            "static let dock = dynamic(0x202020, 0xF0F0F0, alpha: (0.95, 0.95))",
            "extension PlatformColor {",
            "var color: Color {",
            "enum Scale {",
            "    static let heading: CGFloat = 1.74",
        ] {
            assert!(swift.contains(want), "missing {want:?} in:\n{swift}");
        }
        assert!(!swift.contains("selection") && !swift.contains("headingLinux"));
    }

    #[test]
    fn a_colours_alpha_counts_in_its_pairs() {
        let t = tokens(TWO_THEMES).unwrap();
        // Text on the selection: the selection at its alpha over the ground.
        let pair = &t.contrast[1];
        let night = blend([78.0, 161.0, 255.0], [0.0; 3], 0.3);
        assert_eq!(t.ratio(pair, 0), contrast([255.0; 3], night));
        let day = blend([26.0, 115.0, 217.0], [255.0; 3], 0.2);
        assert_eq!(t.ratio(pair, 1), contrast([0.0; 3], day));
        // A translucent colour as bg needs what's under it.
        let errors = tokens(&TWO_THEMES.replace(
            r#""bg": "selection", "bgOver": "ground","#,
            r#""bg": "selection","#,
        ))
        .unwrap_err();
        assert!(errors.join("\n").contains("needs bgOver"), "{errors:?}");
        // And can't be the bottom of a stack.
        let errors =
            tokens(&TWO_THEMES.replace(r#""bgOver": "ground""#, r#""bgOver": "selection""#))
                .unwrap_err();
        assert!(
            errors.join("\n").contains("bottom layer must be opaque"),
            "{errors:?}"
        );
    }

    #[test]
    fn two_theme_swift_and_new_keys_are_validated() {
        let all = |json: String| tokens(&json).unwrap_err().join("\n");
        // Two themes for Swift need the appearance mapping.
        let e = all(TWO_THEMES
            .replace(r#""colorType": "PlatformColor","#, "")
            .replace(r#", "appearance": { "dark": "night", "light": "day" }"#, ""));
        assert!(e.contains("give colorType and appearance"), "{e}");
        let e = all(TWO_THEMES.replace(r#""light": "day""#, r#""light": "noon""#));
        assert!(e.contains("appearance.light must name a theme"), "{e}");
        let e = all(TWO_THEMES.replace(r#""alpha": 0.95"#, r#""alpha": 1.5"#));
        assert!(e.contains("alpha must be above 0"), "{e}");
        let e = all(TWO_THEMES.replace(r#""night": 0.3, "day": 0.2"#, r#""night": 0.3"#));
        assert!(e.contains("for theme \"day\""), "{e}");
        let e = all(TWO_THEMES.replace(r#""only": "swift""#, r#""only": "apple""#));
        assert!(e.contains("\"only\" must be"), "{e}");
        let e = all(TWO_THEMES.replace(r#""value": 1.75"#, r#""value": "1.75""#));
        assert!(e.contains("scale: \"heading-linux\" needs a number"), "{e}");
        // A colour with an alpha also takes <NAME>_ALPHA in Rust.
        let e = all(TWO_THEMES.replace(
            r#"{ "name": "text","#,
            r##"{ "name": "selection-alpha", "value": { "night": "#000000", "day": "#000000" }, "usage": "Clash." },
          { "name": "text","##,
        ));
        assert!(e.contains("\"SELECTION_ALPHA\""), "{e}");
    }

    /// MINIMAL with a translucent `tint` (white at 10%) and `pair`.
    fn with_tint(alpha: &str, pair: &str) -> Result<Tokens, Vec<String>> {
        tokens(
            &MINIMAL
                .replace(
                    r##"{ "name": "dim","##,
                    &format!(
                        r##"{{ "name": "tint", "value": {{ "dark": "#ffffff" }}, "alpha": {alpha}, "usage": "Tint." }},
          {{ "name": "dim","##
                    ),
                )
                .replace(
                    r##"{ "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }"##,
                    pair,
                ),
        )
    }

    #[test]
    fn translucent_colours_carry_their_alpha() {
        let body = r##"{ "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }"##;
        let t = with_tint(r##"{ "dark": 0.1 }"##, body).unwrap();
        let rust = &t.outputs()[0].1;
        assert!(rust.contains("    pub const TINT: u32 = 0xFFFFFF;\n"));
        assert!(
            rust.contains("    pub const TINT_ALPHA: f32 = 0.1;\n"),
            "{rust}"
        );
        assert!(rust.contains("opacity is `<NAME>_ALPHA`"));
        assert!(!rust.contains("GROUND_ALPHA"), "opaque colours have none");
        // The check composites the token at its alpha: text on a 10%
        // white tint over the ground.
        let t = with_tint(
            r##"{ "dark": 0.1 }"##,
            r##"{ "fg": "text", "bg": "tint", "bgOver": "ground", "min": 4.5, "usage": "Chip." }"##,
        )
        .unwrap();
        let fill = blend([255.0; 3], [0.0; 3], 0.1);
        assert_eq!(t.ratio(&t.contrast[0], 0), contrast([255.0; 3], fill));
        // As a foreground, and multiplied by the pair's own alpha.
        let t = with_tint(
            r##"{ "dark": 0.5 }"##,
            r##"{ "fg": "tint", "fgAlpha": 0.5, "bg": "ground", "min": 3, "usage": "Halo." }"##,
        )
        .unwrap();
        let fg = blend([255.0; 3], [0.0; 3], 0.25);
        assert_eq!(t.ratio(&t.contrast[0], 0), contrast(fg, [0.0; 3]));
        let mut printed = Vec::new();
        t.check_contrast(&mut printed);
        assert!(String::from_utf8(printed)
            .unwrap()
            .contains("tint/25 on ground"));
        // A translucent bg needs what's under it, which must be opaque.
        let err = |alpha: &str, pair: &str| with_tint(alpha, pair).unwrap_err().join("\n");
        assert!(err(
            r##"{ "dark": 0.1 }"##,
            r##"{ "fg": "text", "bg": "tint", "min": 4.5, "usage": "Chip." }"##
        )
        .contains("needs bgOver"));
        assert!(err(
            r##"{ "dark": 0.1 }"##,
            r##"{ "fg": "text", "bg": "ground", "bgAlpha": 0.5, "bgOver": "tint", "min": 4.5, "usage": "x" }"##
        )
        .contains("bottom layer must be opaque"));
        // An alpha of 1 everywhere is opaque: no bgOver.
        assert!(err(
            r##"{ "dark": 1 }"##,
            r##"{ "fg": "text", "bg": "tint", "bgOver": "ground", "min": 4.5, "usage": "x" }"##
        )
        .contains("bgOver is only for a translucent bg"));
        // Every theme needs one, in range, and no others.
        assert!(err(r##"{}"##, body)
            .contains("alpha needs a number above 0, up to 1, for theme \"dark\""));
        assert!(err(r##"{ "dark": 0 }"##, body).contains("alpha needs a number"));
        assert!(err(r##"{ "dark": 0.1, "light": 0.2 }"##, body)
            .contains("alpha for unknown theme \"light\""));
        // One number is every theme's alpha; anything else is an error.
        let one = with_tint("0.1", body).unwrap();
        assert_eq!(one.colors[2].alphas, [0.1]);
        assert!(err(r#""x""#, body).contains("alpha needs a number"));
        // `<name>-alpha` would clash with the constant.
        let clash = tokens(&MINIMAL.replace(
            r##"{ "name": "dim", "value": { "dark": "#333333" }, "usage": "Edges." }"##,
            r##"{ "name": "dim", "value": { "dark": "#333333" }, "alpha": { "dark": 0.5 }, "usage": "Edges." },
          { "name": "dim-alpha", "value": { "dark": "#333333" }, "usage": "Edges." }"##,
        ))
        .unwrap_err()
        .join("\n");
        assert!(
            clash.contains("two tokens generate \"DIM_ALPHA\""),
            "{clash}"
        );
        // Swift writes the opacity.
        let swift = tokens(
            &MINIMAL
                .replace(
                    r##""outputs": { "rust": { "path": "src/tokens.rs" } },"##,
                    r##""outputs": { "swift": { "path": "T.swift" } },"##,
                )
                .replace(
                    r##""value": { "dark": "#333333" },"##,
                    r##""value": { "dark": "#333333" }, "alpha": { "dark": 0.5 },"##,
                ),
        )
        .unwrap()
        .outputs()[0]
            .1
            .clone();
        assert!(
            swift.contains("static let dim = Color(hex: 0x333333).opacity(0.5)"),
            "{swift}"
        );
    }

    #[test]
    fn shadows_take_a_spread_and_a_theme_colour() {
        let with_shadow = |outputs: &str, shadow: &str| {
            tokens(&MINIMAL.replace(
                r##""outputs": { "rust": { "path": "src/tokens.rs" } },"##,
                &format!(r##""outputs": {outputs}, "shadow": {{ "tokens": [{shadow}] }},"##),
            ))
        };
        let rust_only = r##"{ "rust": { "path": "src/tokens.rs" } }"##;
        let t = with_shadow(
            rust_only,
            r##"{ "name": "pop", "value": { "x": "0px", "y": "24px", "blur": "60px", "spread": "-12px", "color": "dim" }, "usage": "Dialogs." },
               { "name": "knob", "value": { "x": "0px", "y": "1px", "blur": "2px", "opacity": 0.2 }, "usage": "Knob." }"##,
        )
        .unwrap();
        let rust = &t.outputs()[0].1;
        for want in [
            "        pub spread: f32,\n",
            "    /// Drawn in the theme's `dim` colour.\n",
            "        spread: -12.0,\n        opacity: 1.0,\n",
            "        spread: 0.0,\n        opacity: 0.2,\n",
        ] {
            assert!(rust.contains(want), "missing {want:?} in:\n{rust}");
        }
        // Without a spread anywhere, the struct is as it was.
        let t = with_shadow(
            rust_only,
            r##"{ "name": "knob", "value": { "x": "0px", "y": "1px", "blur": "2px", "opacity": 0.2 }, "usage": "Knob." }"##,
        )
        .unwrap();
        let rust = &t.outputs()[0].1;
        assert!(!rust.contains("spread") && rust.contains("/// Black shadows"));
        // An opacity or a colour, not both or neither; a known colour.
        for bad in [
            r##"{ "x": "0px", "y": "1px", "blur": "2px" }"##,
            r##"{ "x": "0px", "y": "1px", "blur": "2px", "opacity": 0.2, "color": "dim" }"##,
            r##"{ "x": "0px", "y": "1px", "blur": "2px", "color": "nope" }"##,
            r##"{ "x": "0px", "y": "1px", "blur": "2px", "spread": "2", "opacity": 0.2 }"##,
        ] {
            let e = with_shadow(
                rust_only,
                &format!(r##"{{ "name": "s", "value": {bad}, "usage": "x" }}"##),
            )
            .unwrap_err()
            .join("\n");
            assert!(e.contains("either an opacity"), "{bad}: {e}");
        }
        // SwiftUI can't draw either.
        let swift = r##"{ "swift": { "path": "T.swift" } }"##;
        for value in [
            r##"{ "x": "0px", "y": "1px", "blur": "2px", "spread": "3px", "opacity": 0.2 }"##,
            r##"{ "x": "0px", "y": "1px", "blur": "2px", "color": "dim" }"##,
        ] {
            let e = with_shadow(
                swift,
                &format!(r##"{{ "name": "s", "value": {value}, "usage": "x" }}"##),
            )
            .unwrap_err()
            .join("\n");
            assert!(e.contains("SwiftUI shadows"), "{e}");
        }
    }

    #[test]
    fn unicode_escapes_take_four_hex_digits_only() {
        assert_eq!(parse(r#""A""#), Ok(Value::Str("A".into())));
        for bad in [r#""\u+041""#, r#""\u-041""#, r#""\u04g1""#, r#""\u041""#] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn writing_reports_a_folder_it_cant_make() {
        let dir = std::env::temp_dir().join(format!("design-build-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the folder should be.
        std::fs::write(dir.join("taken"), "").unwrap();
        assert!(write_file(&dir.join("taken/out.rs"), "x").is_err());
        write_file(&dir.join("new/out.rs"), "x").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("new/out.rs")).unwrap(),
            "x"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn with_table(table: &str) -> Result<Tokens, Vec<String>> {
        tokens(&MINIMAL.replace(
            r#""contrast""#,
            &format!(r#""tables": [{table}], "contrast""#),
        ))
    }

    const PHASE: &str = r#"{ "name": "phase", "usage": "Parts of the day.",
        "columns": [
          { "name": "starts", "type": "time", "usage": "When it starts." },
          { "name": "sky", "type": "color", "usage": "The sky." },
          { "name": "stroke", "type": "px", "usage": "A line." },
          { "name": "alpha", "type": "number", "usage": "Opacity." },
          { "name": "label", "type": "text", "usage": "Its label." }
        ],
        "rows": [
          { "name": "night", "value": { "starts": "21:00", "sky": "ground", "stroke": "1.5px", "alpha": 0.75, "label": "Night" }, "usage": "Late." },
          { "name": "before-dawn", "value": { "starts": "05:00", "sky": "dim", "stroke": "2px", "alpha": 1, "label": "Before dawn" }, "usage": "Early." }
        ] }"#;

    #[test]
    fn a_table_is_a_rust_module_of_rows() {
        let t = with_table(PHASE).unwrap();
        let rust = &t.outputs()[0].1;
        for want in [
            "/// Parts of the day.\npub mod phase {",
            "    #[derive(Clone, Copy, Debug, PartialEq)]\n    pub struct Row {",
            "        pub name: &'static str,",
            "        /// When it starts. Minutes since midnight.\n        pub starts: u16,",
            "        pub sky: u32,",
            "        pub stroke: f32,",
            "        pub label: &'static str,",
            "    pub const BEFORE_DAWN: Row = Row {\n        name: \"before-dawn\",\n        starts: 300,\n        sky: super::dark::DIM,\n        stroke: 2.0,\n        alpha: 1.0,\n        label: \"Before dawn\",\n    };",
            "        starts: 1260,",
            "        alpha: 0.75,",
            "    pub const ALL: [Row; 2] = [NIGHT, BEFORE_DAWN];\n}",
        ] {
            assert!(rust.contains(want), "missing {want:?} in:\n{rust}");
        }
        // A long list goes one row a line, as rustfmt has it.
        let many = (0..12)
            .map(|n| {
                format!(
                    r#"{{ "name": "row-number-{n}", "value": {{ "at": "00:00" }}, "usage": "x" }}"#
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let t = with_table(&format!(
            r#"{{ "name": "many", "usage": "x", "columns": [{{ "name": "at", "type": "time", "usage": "x" }}], "rows": [{many}] }}"#
        ))
        .unwrap();
        assert!(t.outputs()[0]
            .1
            .contains("    pub const ALL: [Row; 12] = [\n        ROW_NUMBER_0,\n"));
    }

    #[test]
    fn a_pair_takes_its_alpha_from_a_table() {
        let glow = r#"{ "name": "glow", "usage": "x",
            "columns": [{ "name": "alpha", "type": "number", "usage": "x" }, { "name": "at", "type": "time", "usage": "x" }],
            "rows": [{ "name": "dawn", "value": { "alpha": 0.5, "at": "05:00" }, "usage": "x" }] }"#;
        let with = |fg_alpha: &str| {
            tokens(&MINIMAL.replace(
                r#""contrast": [
        { "fg": "text", "bg": "ground", "min": 4.5, "usage": "Body." }
      ]"#,
                &format!(
                    r#""tables": [{glow}], "contrast": [
        {{ "fg": "text", "bg": "ground", "fgAlpha": {fg_alpha}, "min": 3, "usage": "Glow." }}
      ]"#
                ),
            ))
        };
        let t = with(r#""glow.dawn.alpha""#).unwrap();
        assert_eq!(t.contrast[0].fg_alpha, 0.5);
        let by_number = with("0.5").unwrap();
        assert_eq!(
            t.ratio(&t.contrast[0], 0),
            by_number.ratio(&by_number.contrast[0], 0)
        );
        for (bad, want) in [
            (r#""glow.dawn""#, "isn't \"table.row.column\""),
            (r#""nope.dawn.alpha""#, "no table \"nope\""),
            (r#""glow.dusk.alpha""#, "has no row \"dusk\""),
            (r#""glow.dawn.beta""#, "has no column \"beta\""),
            (r#""glow.dawn.at""#, "isn't a number column"),
        ] {
            let all = with(bad).map(|_| ()).unwrap_err().join("\n");
            assert!(all.contains(want), "{bad}: missing {want:?} in:\n{all}");
        }
    }

    #[test]
    fn times_parse() {
        assert_eq!(parse_time("00:00"), Some(0));
        assert_eq!(parse_time("06:30"), Some(390));
        assert_eq!(parse_time("23:59"), Some(1439));
        for bad in ["24:00", "6:30", "06:60", "06-30", "", "aa:bb", "+1:00"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn bad_tables_are_errors() {
        let errs = |table: &str| with_table(table).map(|_| ()).unwrap_err().join("\n");
        let bad = PHASE
            .replace(r#""starts": "21:00""#, r#""starts": "25:00""#)
            .replace(r#""sky": "dim""#, r#""sky": "nope""#)
            .replace(r#""stroke": "2px""#, r#""stroke": "2""#)
            .replace(r#""alpha": 0.75"#, r#""alpha": "x""#)
            .replace(r#""label": "Night""#, r#""label": "a\"b""#)
            .replace(r#""type": "text""#, r#""type": "text", "x": 1"#);
        let all = errs(&bad);
        for want in [
            "row \"night\": \"starts\" must be a time of day",
            "\"sky\" must name a colour token written to Rust; \"nope\" isn't one",
            "\"stroke\" must be a px value",
            "\"alpha\" must be a number",
            "\"label\" must be a string without quotes",
        ] {
            assert!(all.contains(want), "missing {want:?} in:\n{all}");
        }
        // Missing and unknown cells, a repeated row, a bad type.
        let all = errs(
            r#"{ "name": "t", "usage": "x", "columns": [
                 { "name": "a", "type": "time", "usage": "x" },
                 { "name": "b", "type": "colour", "usage": "x" } ],
               "rows": [
                 { "name": "r", "value": { "a": "01:00", "c": 1 }, "usage": "x" },
                 { "name": "r", "value": { "a": "01:00", "b": "x" }, "usage": "x" } ] }"#,
        );
        for want in [
            "column \"b\": type must be color, time, px, number or text",
            "row \"r\": unknown column \"c\"",
            "row \"r\": no value for column \"b\"",
            "row \"r\" is listed twice",
        ] {
            assert!(all.contains(want), "missing {want:?} in:\n{all}");
        }
        assert!(errs(r#"{ "name": "t", "usage": "x" }"#).contains("needs \"columns\""));
        assert!(errs(r#"{ "name": "t", "usage": "x", "columns": [{ "name": "a", "type": "time", "usage": "x" }] }"#)
            .contains("needs \"rows\""));
        // A table's name is a token name: it can't repeat a colour's.
        assert!(
            errs(&PHASE.replace(r#""name": "phase""#, r#""name": "dim""#))
                .contains("\"dim\" is already a token's name")
        );
        // Generated names: a fixed module, the field `name`, a row `all`, a keyword.
        assert!(
            errs(&PHASE.replace(r#""name": "phase""#, r#""name": "size""#))
                .contains("rust top level: two tokens generate \"size\"")
        );
        assert!(errs(
            &PHASE
                .replace(r#""name": "label""#, r#""name": "name""#)
                .replace(r#""label":"#, r#""name":"#)
        )
        .contains("rust phase::Row: two tokens generate \"name\""));
        assert!(
            errs(&PHASE.replace(r#""name": "night""#, r#""name": "all""#))
                .contains("rust phase: two tokens generate \"ALL\"")
        );
        assert!(errs(
            &PHASE
                .replace(r#""name": "label""#, r#""name": "type""#)
                .replace(r#""label":"#, r#""type":"#)
        )
        .contains("\"type\" is a reserved word"));
        // A color column with two themes, and tables without a Rust output.
        let two = MINIMAL
            .replace(
                r#"[{ "id": "dark", "name": "Dark" }]"#,
                r#"[{ "id": "dark" }, { "id": "light" }]"#,
            )
            .replace(r##""#000000" }"##, r##""#000000", "light": "#000000" }"##)
            .replace(r##""#ffffff" }"##, r##""#ffffff", "light": "#ffffff" }"##)
            .replace(r##""#333333" }"##, r##""#333333", "light": "#333333" }"##)
            .replace(
                r#""contrast""#,
                &format!(r#""tables": [{PHASE}], "contrast""#),
            );
        assert!(tokens(&two)
            .map(|_| ())
            .unwrap_err()
            .join("\n")
            .contains("needs exactly one theme"));
        let swift_only = MINIMAL
            .replace(
                r#"{ "rust": { "path": "src/tokens.rs" } }"#,
                r#"{ "swift": { "path": "T.swift" } }"#,
            )
            .replace(
                r#""contrast""#,
                &format!(r#""tables": [{PHASE}], "contrast""#),
            );
        assert!(tokens(&swift_only)
            .map(|_| ())
            .unwrap_err()
            .join("\n")
            .contains("they need outputs.rust"));
    }

    #[test]
    fn durations_round_to_the_millisecond() {
        // Floating point makes 1.001 s 1000.9999999999999 ms.
        assert_eq!(parse_motion("1.001s"), Some(Motion::Millis(1001)));
        assert_eq!(parse_motion("0.007s"), Some(Motion::Millis(7)));
        assert_eq!(parse_motion("2.345s"), Some(Motion::Millis(2345)));
        // A real fraction of a millisecond is still an error.
        assert_eq!(parse_motion("1.0005s"), None);
        assert_eq!(parse_motion("0.5ms"), None);
        assert_eq!(parse_motion("1e30s"), None);
        assert_eq!(parse_motion("-1ms"), None);
    }

    #[test]
    fn numbers_stay_within_f32() {
        assert_eq!(parse_px("1e39px"), None);
        assert_eq!(parse_px("infpx"), None);
        assert_eq!(parse_px("NaNpx"), None);
        assert_eq!(parse_px_signed("-1e39px"), None);
        assert_eq!(parse_px("3e38px"), Some(3e38));
        assert_eq!(parse_motion("cubic-bezier(0.4, 1e39, 0.2, 1)"), None);
        let errs = |json: String| tokens(&json).map(|_| ()).unwrap_err().join("\n");
        let spacing = MINIMAL.replace(
            r#""contrast""#,
            r#""spacing": { "tokens": [{ "name": "huge", "value": "1e39px", "usage": "x" }] }, "contrast""#,
        );
        assert!(errs(spacing).contains("\"huge\" needs a px value within f32's range"));
        let table = MINIMAL.replace(
            r#""contrast""#,
            r#""tables": [{ "name": "t", "usage": "x",
                 "columns": [{ "name": "n", "type": "number", "usage": "x" }, { "name": "w", "type": "px", "usage": "x" }],
                 "rows": [{ "name": "r", "value": { "n": 1e39, "w": "1e39px" }, "usage": "x" }] }], "contrast""#,
        );
        let all = errs(table);
        assert!(
            all.contains("\"n\" must be a number within f32's range"),
            "{all}"
        );
        assert!(
            all.contains("\"w\" must be a px value within f32's range"),
            "{all}"
        );
    }

    #[test]
    fn the_version_is_in_every_header() {
        assert_eq!(VERSION, format!("design-gen {}", env!("CARGO_PKG_VERSION")));
        assert!(HEADER.contains(&format!("({VERSION})")), "{HEADER}");
        let t = full().unwrap();
        for (path, contents) in t.outputs() {
            let first = contents.lines().next().unwrap();
            assert!(first.ends_with(HEADER), "{path}: {first}");
        }
    }

    #[test]
    fn tiny_fractions_of_a_millisecond_are_errors() {
        // Below any rounding error, but still not a whole millisecond.
        assert_eq!(parse_motion("0.0000001ms"), None);
        assert_eq!(parse_motion("0.0000000001s"), None);
        assert_eq!(parse_motion("1000.0000001ms"), None);
        assert_eq!(parse_motion("0ms"), Some(Motion::Millis(0)));
        assert_eq!(
            parse_motion("4294967.295s"),
            Some(Motion::Millis(4_294_967_295))
        );
        // However large the value: no tolerance grows with it.
        assert_eq!(parse_motion("4294967.2949s"), None);
        assert_eq!(parse_motion("4294967294.9999999ms"), None);
        assert_eq!(parse_motion("4294967.296s"), None, "over u32::MAX");
        assert_eq!(parse_motion("1.000000000000000000001s"), None);
        assert_eq!(parse_motion("1.0010000s"), Some(Motion::Millis(1001)));
        for bad in [".5s", "1.s", "+1ms", "1e3ms", "", "s", "ms", "1,5s"] {
            assert_eq!(parse_motion(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_byte_order_mark_is_a_clear_error() {
        let e = parse("\u{feff}{}").unwrap_err();
        assert!(e.contains("byte order mark"), "{e}");
    }

    #[test]
    fn the_printed_ratio_is_the_compared_one() {
        assert_eq!(shown_ratio(4.4999, 4.5), 4.49, "never rounds up to pass");
        assert_eq!(shown_ratio(4.4999, 3.0), 4.5);
        assert_eq!(shown_ratio(4.5, 4.5), 4.5);
        assert_eq!(shown_ratio(1.666, 3.0), 1.67);
        for r in [2.994, 2.995, 2.9999, 3.0, 4.494, 4.495, 4.5001, 6.996, 7.0] {
            for min in [3.0, 4.5, 7.0] {
                assert_eq!(shown_ratio(r, min) >= min, r >= min, "{r} against {min}");
            }
        }
        // dim on ground is 1.662…:1; the printout and the comparison with
        // `was` both use 1.66.
        let t = with_pair(
            r#"{ "fg": "dim", "bg": "ground", "min": 3, "usage": "Edge.", "pending": "Fix.", "ticket": "ALI-1", "was": 1.66 }"#,
        );
        let mut out = Vec::new();
        assert_eq!(t.check_contrast(&mut out), (false, 1));
        assert!(String::from_utf8(out).unwrap().contains(" 1.66:1 (min 3)"));
    }

    #[test]
    fn all_is_laid_out_as_rustfmt_does() {
        let names = |n: usize, f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<Vec<_>>();
        // Fits: one line.
        assert_eq!(
            all_rows(&names(3, &|i| format!("ROW_{i}"))),
            "    pub const ALL: [Row; 3] = [ROW_0, ROW_1, ROW_2];\n"
        );
        // Short names (10 characters or fewer) are packed, 99 columns a line.
        let packed = all_rows(&names(30, &|i| format!("ROW_{i}")));
        assert_eq!(
            packed,
            "    pub const ALL: [Row; 30] = [\n        \
             ROW_0, ROW_1, ROW_2, ROW_3, ROW_4, ROW_5, ROW_6, ROW_7, ROW_8, ROW_9, ROW_10, ROW_11,\n        \
             ROW_12, ROW_13, ROW_14, ROW_15, ROW_16, ROW_17, ROW_18, ROW_19, ROW_20, ROW_21, ROW_22,\n        \
             ROW_23, ROW_24, ROW_25, ROW_26, ROW_27, ROW_28, ROW_29,\n    ];\n"
        );
        // Over 60 columns but one line of 100 inside the brackets.
        let one = all_rows(&names(15, &|i| format!("ROW_{i:02}")));
        assert!(one.contains("[\n        ROW_00, ROW_01,") && one.contains("ROW_14,\n    ];"));
        // A longer name: one a line.
        let mut long = names(5, &|i| format!("ROW_{i}"));
        long.push("A_LONGER_NAME".into());
        long.extend(names(8, &|i| format!("MORE_{i}")));
        assert!(all_rows(&long).contains("[\n        ROW_0,\n        ROW_1,\n"));
    }

    #[test]
    fn only_keeps_references_on_the_platform_they_reach() {
        // A table's colour must be written to Rust.
        assert!(with_table(&PHASE.replace(r#""sky": "dim""#, r#""sky": "text""#)).is_ok());
        let swift_text = MINIMAL.replace(
            r##"{ "name": "text", "value": { "dark": "#ffffff" }, "usage": "Text." }"##,
            r##"{ "name": "text", "value": { "dark": "#ffffff" }, "only": "swift", "usage": "Text." }"##,
        );
        let table = swift_text.replace(
            r#""contrast""#,
            &format!(
                r#""tables": [{}], "contrast""#,
                PHASE.replace(r#""sky": "dim""#, r#""sky": "text""#)
            ),
        );
        let e = tokens(&table).map(|_| ()).unwrap_err().join("\n");
        assert!(e.contains("written to Rust; \"text\" isn't one"), "{e}");
        // So must a Rust shadow's.
        let shadow = swift_text.replace(
            r#""contrast""#,
            r#""shadow": { "tokens": [{ "name": "lift", "value": { "x": "0px", "y": "1px", "blur": "2px", "color": "text" }, "usage": "x" }] }, "contrast""#,
        );
        let e = tokens(&shadow).map(|_| ()).unwrap_err().join("\n");
        assert!(e.contains("its colour isn't written to Rust"), "{e}");
        // A spread shadow kept out of Swift is fine with a Swift output.
        let both = MINIMAL.replace(
            r#""outputs": { "rust": { "path": "src/tokens.rs" } },"#,
            r#""outputs": { "rust": { "path": "src/tokens.rs" }, "swift": { "path": "T.swift" } },
               "shadow": { "tokens": [{ "name": "pop", "value": { "x": "0px", "y": "8px", "blur": "24px", "spread": "-4px", "color": "dim" }, "only": "rust", "usage": "x" }] },"#,
        );
        let t = tokens(&both).unwrap();
        let out = t.outputs();
        assert!(out[0].1.contains("spread: -4.0,"));
        assert!(!out[1].1.contains("ShadowToken"));
    }

    #[test]
    fn minimums_have_two_decimals_at_most() {
        // shown_ratio's promise (shown >= min exactly when the ratio is)
        // needs every min to be a whole number of hundredths.
        for min in MINIMUMS {
            assert_eq!(two_places(min), min);
            assert_eq!((min * 100.0).fract(), 0.0);
        }
        let e = tokens(&MINIMAL.replace(r#""min": 4.5"#, r#""min": 4.505"#))
            .map(|_| ())
            .unwrap_err()
            .join("\n");
        assert!(e.contains("min must be 3, 4.5 or 7"), "{e}");
    }

    #[test]
    fn pending_pairs_compare_the_shown_ratio_with_was() {
        // White at 34.89% on black is 2.9966:1. Rounded it would read 3.00;
        // just under min 3 it's cut to 2.99, which is what was meets.
        let pair = |was: &str| {
            with_pair(&format!(
                r#"{{ "fg": "text", "fgAlpha": 0.3489, "bg": "ground", "min": 3, "usage": "x", "pending": "p", "ticket": "ALI-1", "was": {was} }}"#
            ))
        };
        let t = pair("3.0");
        let r = t.ratio(&t.contrast[0], 0);
        assert!(r > 2.996 && r < 3.0, "{r}");
        let mut out = Vec::new();
        assert!(t.check_contrast(&mut out).0, "2.99 is worse than 3.0");
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(" 2.99:1 (min 3)") && out.contains("Worse than the 3:1"),
            "{out}"
        );
        assert_eq!(pair("2.99").check_contrast(&mut Vec::new()), (false, 1));
    }

    #[test]
    fn a_row_is_one_line_when_rustfmt_would_keep_it_so() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // 18 columns of fields: one line.
        assert_eq!(
            row_literal("R0", &f(&["name: \"r0\"", "ab: 12"])),
            "    pub const R0: Row = Row { name: \"r0\", ab: 12 };\n"
        );
        // 19: one a line.
        assert_eq!(
            row_literal("R0", &f(&["name: \"r0\"", "ab: 1.0"])),
            "    pub const R0: Row = Row {\n        name: \"r0\",\n        ab: 1.0,\n    };\n"
        );
    }

    #[test]
    fn a_table_colour_cant_have_an_alpha() {
        let json = MINIMAL
            .replace(
                r##""value": { "dark": "#333333" },"##,
                r##""value": { "dark": "#333333" }, "alpha": 0.5,"##,
            )
            .replace(
                r#""contrast""#,
                &format!(r#""tables": [{PHASE}], "contrast""#),
            );
        let e = tokens(&json).map(|_| ()).unwrap_err().join("\n");
        assert!(e.contains("names \"dim\", which has an alpha"), "{e}");
    }

    #[test]
    fn swift_reserves_the_appearance_helper() {
        let e = tokens(&TWO_THEMES.replace(r#""name": "accent","#, r#""name": "dynamic","#))
            .map(|_| ())
            .unwrap_err()
            .join("\n");
        assert!(
            e.contains("Palette: two tokens generate \"dynamic\""),
            "{e}"
        );
        // A single-theme SwiftUI output has no helper, so the name is free.
        let swift = MINIMAL
            .replace(
                r#"{ "rust": { "path": "src/tokens.rs" } }"#,
                r#"{ "swift": { "path": "T.swift" } }"#,
            )
            .replace(r#""name": "dim""#, r#""name": "dynamic""#);
        assert!(tokens(&swift).is_ok());
    }

    /// MINIMAL with only a CSS output.
    fn css_only(extra: &str) -> Result<Tokens, Vec<String>> {
        tokens(
            &MINIMAL
                .replace(
                    r#"{ "rust": { "path": "src/tokens.rs" } }"#,
                    r#"{ "css": { "path": "site/tokens.css" } }"#,
                )
                .replace(r#""contrast""#, &format!(r#"{extra}"contrast""#)),
        )
    }

    fn css_of(t: &Tokens) -> String {
        let out = t.outputs();
        let css = out.iter().find(|(p, _)| p.ends_with(".css"));
        css.expect("a CSS output").1.clone()
    }

    #[test]
    fn css_writes_one_theme_as_custom_properties() {
        let t = css_only(
            r#""type": {
                 "families": [{ "name": "mono", "value": "JetBrains Mono", "css": "ui-monospace, monospace", "usage": "Mono." }],
                 "groups": [{ "name": "web", "usage": "Site.", "styles": [
                   { "name": "hero", "fontSize": "56px", "fontWeight": 300, "lineHeight": 1.05, "letterSpacing": "-0.02em", "usage": "Hero." },
                   { "name": "label", "fontSize": "11px", "lineHeight": "16px", "letterSpacing": "0", "usage": "Label." }
                 ] }]
               },
               "spacing": { "tokens": [{ "name": "gap", "value": "6px", "usage": "Gap." }] },
               "radius": { "tokens": [{ "name": "round", "value": "4.5px", "usage": "Corners." }] },
               "size": { "tokens": [{ "name": "tap", "value": "44px", "usage": "Targets." }] },
               "shadow": { "tokens": [
                 { "name": "lift", "value": { "x": "0px", "y": "-2px", "blur": "8px", "opacity": 0.25 }, "usage": "Lift." },
                 { "name": "glow", "value": { "x": "0px", "y": "0px", "blur": "4px", "spread": "-1px", "color": "dim" }, "usage": "Glow." }
               ] },
               "motion": { "tokens": [
                 { "name": "glide", "value": "0.26s", "usage": "Glide." },
                 { "name": "ease", "value": "cubic-bezier(0.22, 1, 0.36, 1)", "usage": "Ease." }
               ] },
               "scale": { "tokens": [
                 { "name": "share", "value": 0.3, "usage": "Share." },
                 { "name": "most", "value": 2.0, "usage": "Most." }
               ] },
               "#,
        )
        .unwrap();
        let out = t.outputs();
        assert_eq!(out.len(), 1, "CSS alone is an output");
        let css = &out[0].1;
        assert!(
            css.starts_with(&format!("/* {HEADER} */\n\n:root {{\n")),
            "{css}"
        );
        assert!(css.ends_with("}\n"), "{css}");
        for want in [
            "  /* Colours: Dark. */\n  /* Background. */\n  --ground: #000000;\n",
            "  --text: #ffffff;\n",
            "  --font-mono: \"JetBrains Mono\", ui-monospace, monospace;\n",
            "  --hero-size: 3.5rem;\n  --hero-weight: 300;\n  --hero-line-height: 1.05;\n  --hero-letter-spacing: -0.02em;\n",
            "  --label-size: 0.6875rem;\n  --label-line-height: 1rem;\n  --label-letter-spacing: 0;\n",
            "  --gap: 6px;\n",
            "  --round: 4.5px;\n",
            "  --tap: 44px;\n",
            "  --lift: 0px -2px 8px rgb(0 0 0 / 0.25);\n",
            "  --glow: 0px 0px 4px -1px var(--dim);\n",
            "  --glide: 260ms;\n",
            "  --ease: cubic-bezier(0.22, 1, 0.36, 1);\n",
            "  --share: 0.3;\n",
            "  --most: 2;\n",
        ] {
            assert!(css.contains(want), "missing {want:?} in:\n{css}");
        }
        // A style without a weight writes none; one theme has no media block.
        assert!(!css.contains("--label-weight"));
        assert!(!css.contains("@media"));
        // The selector can be given.
        let t = tokens(&MINIMAL.replace(
            r#"{ "rust": { "path": "src/tokens.rs" } }"#,
            r#"{ "css": { "path": "t.css", "selector": ".calc", "note": "For the site." } }"#,
        ))
        .unwrap();
        let css = css_of(&t);
        assert!(css.contains("/* For the site. */\n\n.calc {\n"), "{css}");
    }

    #[test]
    fn css_writes_two_themes_by_colour_scheme() {
        let two = TWO_THEMES.replace(
            r#""rust": { "path": "src/tokens.rs" },"#,
            r#""rust": { "path": "src/tokens.rs" },
               "css": { "path": "t.css", "appearance": { "dark": "night", "light": "day" } },"#,
        );
        let css = css_of(&tokens(&two).unwrap());
        // Dark under the selector, every colour written to CSS.
        for want in [
            ":root {\n  /* Colours: Night (dark mode). */\n",
            "  --ground: #000000;\n",
            "  --text: #ffffff;\n",
            "  --accent: #4ea1ff;\n",
        ] {
            assert!(css.contains(want), "missing {want:?} in:\n{css}");
        }
        // Light in a media block, with only the colours that change.
        let light = "\n@media (prefers-color-scheme: light) {\n  :root {\n    \
                     /* Colours: Day (light mode), where they differ. */\n    \
                     --ground: #ffffff;\n    --text: #000000;\n  }\n}\n";
        assert!(css.ends_with(light), "{css}");
        // Rust- and Swift-only colours aren't written.
        for gone in ["--dock", "--selection", "--scrollbar", "--heading"] {
            assert!(!css.contains(gone), "{gone} in:\n{css}");
        }
        // Swapped, light is the base and dark the override.
        let swapped = css_of(
            &tokens(&two.replace(
                r#"{ "dark": "night", "light": "day" } },"#,
                r#"{ "dark": "day", "light": "night" } },"#,
            ))
            .unwrap(),
        );
        assert!(
            swapped.contains("Day (dark mode)") && swapped.contains("    --ground: #000000;\n")
        );
        // Two themes need appearance, naming real themes.
        let e = |json: String| tokens(&json).map(|_| ()).unwrap_err().join("\n");
        let missing = e(two.replace(
            r#", "appearance": { "dark": "night", "light": "day" } },"#,
            " },",
        ));
        assert!(
            missing.contains("outputs.css: with more than one theme, give appearance"),
            "{missing}"
        );
        let unknown = e(two.replace(
            r#"{ "dark": "night", "light": "day" } },"#,
            r#"{ "dark": "night", "light": "dusk" } },"#,
        ));
        assert!(
            unknown.contains("outputs.css.appearance.light must name a theme (\"dusk\""),
            "{unknown}"
        );
    }

    #[test]
    fn only_web_reaches_the_css_alone() {
        let json = MINIMAL
            .replace(
                r#""outputs": { "rust": { "path": "src/tokens.rs" } },"#,
                r#""outputs": { "rust": { "path": "src/tokens.rs" }, "swift": { "path": "T.swift" }, "css": { "path": "t.css" } },
                   "type": { "groups": [
                     { "name": "app", "usage": "App.", "styles": [{ "name": "result", "fontSize": "60px", "usage": "Result." }] },
                     { "name": "site", "only": "web", "usage": "Site.", "styles": [{ "name": "hero", "fontSize": "56px", "usage": "Hero." }] },
                     { "name": "native", "only": "rust", "usage": "Native.", "styles": [{ "name": "caption", "fontSize": "12px", "usage": "Caption." }] }
                   ] },
                   "spacing": { "tokens": [
                     { "name": "space-4", "value": "16px", "only": "web", "usage": "Site." },
                     { "name": "pad", "value": "12px", "usage": "Both." },
                     { "name": "gutter", "value": "20px", "only": "swift", "usage": "Apple." }
                   ] },
                   "motion": { "tokens": [{ "name": "fade", "value": "150ms", "only": "web", "usage": "Site." }] },
                   "scale": { "tokens": [{ "name": "zoom", "value": 1.5, "only": "rust", "usage": "Rust." }] },"#,
            )
            .replace(
                r##"{ "name": "dim", "value": { "dark": "#333333" }, "usage": "Edges." }"##,
                r##"{ "name": "dim", "value": { "dark": "#333333" }, "only": "web", "usage": "Edges." }"##,
            );
        let t = tokens(&json).unwrap();
        let out = t.outputs();
        let (rust, swift, css) = (&out[0].1, &out[1].1, &out[2].1);
        for gone in ["DIM", "SPACE_4", "FADE", "pub mod site", "HERO"] {
            assert!(!rust.contains(gone), "{gone} in:\n{rust}");
        }
        assert!(rust.contains("pub const PAD: f32 = 12.0;"));
        assert!(rust.contains("pub mod app {") && rust.contains("pub mod native {"));
        assert!(rust.contains("pub const ZOOM: f32 = 1.5;"));
        for gone in ["dim", "space4", "fade"] {
            assert!(!swift.contains(gone), "{gone} in:\n{swift}");
        }
        assert!(swift.contains("static let pad: CGFloat = 12"));
        for want in [
            "--dim: #333333;",
            "--space-4: 16px;",
            "--pad: 12px;",
            "--fade: 150ms;",
            "--result-size: 3.75rem;",
            "--hero-size: 3.5rem;",
        ] {
            assert!(css.contains(want), "missing {want:?} in:\n{css}");
        }
        for gone in ["--gutter", "--zoom", "--caption"] {
            assert!(!css.contains(gone), "{gone} in:\n{css}");
        }
        // A group written to Swift can't be for another output.
        let e = tokens(&json.replace(
            r#"{ "name": "site", "only": "web","#,
            r#"{ "name": "site", "only": "web", "swift": "Site","#,
        ))
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(e.contains("\"site\": a group with a swift name"), "{e}");
        let e = css_only(r#""spacing": { "tokens": [{ "name": "x", "value": "1px", "only": "css", "usage": "x" }] }, "#)
            .map(|_| ())
            .unwrap_err()
            .join("\n");
        assert!(
            e.contains("\"only\" must be \"rust\", \"swift\" or \"web\""),
            "{e}"
        );
    }

    #[test]
    fn rem_is_px_over_sixteen_trimmed() {
        assert_eq!(rem(56.0), "3.5rem");
        assert_eq!(rem(16.0), "1rem");
        assert_eq!(rem(13.0), "0.8125rem");
        assert_eq!(rem(31.2), "1.95rem");
        assert_eq!(rem(0.1), "0.00625rem");
        assert_eq!(css_num(-0.0), "0");
        assert_eq!(css_num(2.0), "2");
        assert_eq!(css_num(-0.02), "-0.02");
    }

    #[test]
    fn a_family_takes_a_css_fallback() {
        let family = |f: &str| {
            css_only(&format!(r#""type": {{ "families": [{f}] }}, "#)).map(|t| css_of(&t))
        };
        let css = family(r#"{ "name": "mono", "value": "JetBrains Mono", "css": "ui-monospace, \"Cascadia Mono\", monospace", "usage": "x" }"#).unwrap();
        assert!(
            css.contains(
                "  --font-mono: \"JetBrains Mono\", ui-monospace, \"Cascadia Mono\", monospace;\n"
            ),
            "{css}"
        );
        let css = family(r#"{ "name": "serif", "value": "Newsreader", "usage": "x" }"#).unwrap();
        assert!(css.contains("  --font-serif: \"Newsreader\";\n"), "{css}");
        for bad in [
            "",
            "serif; color: red",
            "a } b",
            "\"unpaired, serif",
            "a /* b */",
        ] {
            let e = family(&format!(
                r#"{{ "name": "m", "value": "M", "css": "{}", "usage": "x" }}"#,
                bad.replace('"', "\\\"")
            ))
            .unwrap_err()
            .join("\n");
            assert!(e.contains("css must be a fallback list"), "{bad:?}: {e}");
        }
        // A family's property shares the names with the tokens.
        let e = css_only(
            r#""type": { "families": [{ "name": "mono", "value": "M", "usage": "x" }] },
               "size": { "tokens": [{ "name": "font-mono", "value": "1px", "usage": "x" }] }, "#,
        )
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(
            e.contains("css: two tokens generate \"--font-mono\""),
            "{e}"
        );
    }

    #[test]
    fn css_colours_carry_their_alpha() {
        let t = tokens(
            &MINIMAL
                .replace(
                    r#"{ "rust": { "path": "src/tokens.rs" } }"#,
                    r#"{ "css": { "path": "t.css" } }"#,
                )
                .replace(
                    r##"{ "name": "dim", "value": { "dark": "#333333" }, "usage": "Edges." }"##,
                    r##"{ "name": "dim", "value": { "dark": "#3a4a68" }, "alpha": 0.35, "usage": "Edges." },
                        { "name": "solid", "value": { "dark": "#3a4a68" }, "alpha": 1, "usage": "Opaque." }"##,
                ),
        )
        .unwrap();
        let css = css_of(&t);
        assert!(css.contains("  --dim: rgb(58 74 104 / 0.35);\n"), "{css}");
        assert!(css.contains("  --solid: #3a4a68;\n"), "{css}");
    }

    #[test]
    fn line_height_and_letter_spacing_are_validated() {
        let style = |st: &str| {
            css_only(&format!(
                r#""type": {{ "groups": [{{ "name": "web", "usage": "x", "styles": [{st}] }}] }}, "#
            ))
            .map(|_| ())
            .map_err(|e| e.join("\n"))
        };
        assert!(style(r#"{ "name": "a", "fontSize": "14px", "lineHeight": 1.5, "letterSpacing": "0.5px", "usage": "x" }"#).is_ok());
        for bad in [
            r#"0"#,
            r#"-1"#,
            r#""1.5""#,
            r#""1.5em""#,
            r#""0px""#,
            r#"true"#,
        ] {
            let e = style(&format!(
                r#"{{ "name": "a", "fontSize": "14px", "lineHeight": {bad}, "usage": "x" }}"#
            ))
            .unwrap_err();
            assert!(e.contains("lineHeight must be"), "{bad}: {e}");
        }
        for bad in [r#""-0.02""#, r#""1rem""#, r#""em""#, r#"0"#, r#""1e39em""#] {
            let e = style(&format!(
                r#"{{ "name": "a", "fontSize": "14px", "letterSpacing": {bad}, "usage": "x" }}"#
            ))
            .unwrap_err();
            assert!(e.contains("letterSpacing must be"), "{bad}: {e}");
        }
        assert_eq!(parse_letter_spacing("-0.02em"), Some(Spacing::Em(-0.02)));
        assert_eq!(parse_letter_spacing("0"), Some(Spacing::Em(0.0)));
        assert_eq!(parse_letter_spacing("-1px"), Some(Spacing::Px(-1.0)));
        // Style names repeat across groups, but not in the CSS output.
        let e = css_only(
            r#""type": { "groups": [
                 { "name": "app", "usage": "x", "styles": [{ "name": "body", "fontSize": "14px", "usage": "x" }] },
                 { "name": "web", "usage": "x", "styles": [{ "name": "body", "fontSize": "16px", "usage": "x" }] }
               ] }, "#,
        )
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(
            e.contains("css: two tokens generate \"--body-size\""),
            "{e}"
        );
        // Nor may a style's property meet a token's.
        let e = css_only(
            r#""type": { "groups": [{ "name": "app", "usage": "x", "styles": [{ "name": "key", "fontSize": "14px", "usage": "x" }] }] },
               "size": { "tokens": [{ "name": "key-size", "value": "40px", "usage": "x" }] }, "#,
        )
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(e.contains("css: two tokens generate \"--key-size\""), "{e}");
    }

    #[test]
    fn css_output_is_checked_like_the_others() {
        // Tables stay Rust only.
        let e = css_only(&format!(r#""tables": [{PHASE}], "#))
            .map(|_| ())
            .unwrap_err()
            .join("\n");
        assert!(e.contains("they need outputs.rust"), "{e}");
        // No output at all names all three.
        let e = tokens(&MINIMAL.replace(r#"{ "rust": { "path": "src/tokens.rs" } }"#, "{}"))
            .map(|_| ())
            .unwrap_err()
            .join("\n");
        assert!(
            e.contains("name at least one output (rust, swift, css)"),
            "{e}"
        );
        // The path and selector are validated.
        let e = tokens(&MINIMAL.replace(
            r#"{ "rust": { "path": "src/tokens.rs" } }"#,
            r#"{ "css": { "path": "../t.css", "selector": "a { color: red } b" } }"#,
        ))
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(
            e.contains("outputs.css.path must be a path inside the repo"),
            "{e}"
        );
        let e = tokens(&MINIMAL.replace(
            r#"{ "rust": { "path": "src/tokens.rs" } }"#,
            r#"{ "css": { "path": "t.css", "selector": "a { color: red } b" } }"#,
        ))
        .map(|_| ())
        .unwrap_err()
        .join("\n");
        assert!(e.contains("outputs.css.selector"), "{e}");
        // A CSS shadow's colour must be written to CSS.
        let e = css_only(
            r#""shadow": { "tokens": [{ "name": "lift", "value": { "x": "0px", "y": "1px", "blur": "2px", "color": "dim" }, "usage": "x" }] }, "#,
        )
        .map(|t| css_of(&t))
        .unwrap();
        assert!(e.contains("--lift: 0px 1px 2px var(--dim);"), "{e}");
        let rust_dim = MINIMAL
            .replace(
                r#"{ "rust": { "path": "src/tokens.rs" } }"#,
                r#"{ "css": { "path": "t.css" } }"#,
            )
            .replace(
                r##""usage": "Edges." }"##,
                r##""only": "rust", "usage": "Edges." }"##,
            )
            .replace(
                r#""contrast""#,
                r#""shadow": { "tokens": [{ "name": "lift", "value": { "x": "0px", "y": "1px", "blur": "2px", "color": "dim" }, "usage": "x" }] }, "contrast""#,
            );
        let e = tokens(&rust_dim).map(|_| ()).unwrap_err().join("\n");
        assert!(e.contains("its colour isn't written to CSS"), "{e}");
        // A usage note can't end its comment early.
        let t = css_only(r#""size": { "tokens": [{ "name": "tap", "value": "44px", "usage": "Targets */ x" }] }, "#).unwrap();
        assert!(css_of(&t).contains("/* Targets * / x */"));
    }
}

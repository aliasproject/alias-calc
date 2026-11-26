//! The aliasOS text size. `aliasos-display-text-size` writes it as
//! `base-size` (in px, 9–20, default 12) under `[font]` in
//! `~/.config/aliasos/shell.toml`, and sets GNOME's text-scaling-factor to
//! match. Alias Calc scales its text and its starting window by
//! base-size ÷ 12 (WCAG 1.4.4: text resizes to 200%).

use std::path::PathBuf;

/// The aliasOS default, which Alias Calc's sizes are designed at.
const DEFAULT_PX: f32 = 12.0;

/// The smallest and largest factors followed. aliasOS's own range (9–20 px)
/// is 0.75–1.67; 2.0 leaves room for a hand-edited file.
const MIN_SCALE: f32 = 0.75;
const MAX_SCALE: f32 = 2.0;

/// The folder holding `shell.toml`, which the theme watch also watches.
pub fn dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".config/aliasos"))
}

/// The file name a change to watch for in `dir()`.
pub const FILE: &str = "shell.toml";

/// The current text scale: 1.0 when there's no setting or it can't be read.
pub fn load() -> f32 {
    dir()
        .and_then(|d| std::fs::read_to_string(d.join(FILE)).ok())
        .and_then(|text| base_size(&text))
        .map_or(1.0, |px| (px / DEFAULT_PX).clamp(MIN_SCALE, MAX_SCALE))
}

/// `base-size` under `[font]`, as aliasos-display-text-size writes it.
fn base_size(text: &str) -> Option<f32> {
    let mut in_font = false;
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(table) = line.strip_prefix('[') {
            in_font = table.trim_end_matches(']').trim() == "font";
            continue;
        }
        if !in_font {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "base-size"
        {
            return value
                .trim()
                .trim_matches('"')
                .parse::<f32>()
                .ok()
                .filter(|v| *v > 0.0);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_base_size_under_font_only() {
        assert_eq!(base_size("[font]\nbase-size = 18\n"), Some(18.0));
        assert_eq!(
            base_size("[bar]\nbase-size = 30\n[font]\nbase-size = 14 # px\n"),
            Some(14.0)
        );
        assert_eq!(base_size("base-size = 18\n"), None, "not under [font]");
        assert_eq!(base_size("[font]\nfamily = \"x\"\n"), None);
        assert_eq!(base_size("[font]\nbase-size = big\n"), None);
        assert_eq!(base_size("[font]\nbase-size = 0\n"), None);
    }
}

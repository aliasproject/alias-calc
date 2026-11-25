//! The live aliasOS theme: colors from `current/theme/colors.toml`, re-read
//! whenever the theme changes.
//!
//! `aliasos-theme-apply` swaps the theme directory in with `mv`, which fires
//! `IN_MOVED_TO` for `theme` on its parent (the same event
//! `aliasos-theme-changed` waits on). The theme directory itself briefly
//! stops existing during the swap, so the parent is what's watched. Omarchy
//! swaps a `theme` symlink (`IN_CREATE`) in `~/.config/omarchy/current`; the
//! same watch covers it.

use std::ffi::OsStr;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

use crate::render::Color;
use crate::ui::Palette;

/// Where a theme lives: the `current` directory that holds `theme/`.
fn sources() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"));
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    vec![
        state.join("aliasos/current"),
        config.join("omarchy/current"),
    ]
}

/// The first theme source that exists: aliasOS, then Omarchy.
fn current_dir() -> Option<PathBuf> {
    sources().into_iter().find(|dir| dir.is_dir())
}

/// Reads the palette from the current theme, or the built-in one when
/// there's no theme or it can't be read.
pub fn load() -> Palette {
    current_dir()
        .and_then(|dir| std::fs::read_to_string(dir.join("theme/colors.toml")).ok())
        .and_then(|text| palette_from(&text))
        .unwrap_or_default()
}

/// The palette a `colors.toml` describes. It needs `background` and
/// `foreground`; `accent` falls back to `color4` (blue), then to the
/// foreground.
fn palette_from(text: &str) -> Option<Palette> {
    let mut background = None;
    let mut foreground = None;
    let mut accent = None;
    let mut blue = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(color) = color_value(value) else {
            continue;
        };
        match key.trim() {
            "background" => background = Some(color),
            "foreground" => foreground = Some(color),
            "accent" => accent = Some(color),
            "color4" => blue = Some(color),
            _ => {}
        }
    }
    let foreground = foreground?;
    Some(Palette::new(
        background?,
        foreground,
        accent.or(blue).unwrap_or(foreground),
    ))
}

/// `"#rrggbb"`, with optional quotes and a trailing `# comment`.
fn color_value(value: &str) -> Option<Color> {
    let value = value.trim();
    let value = match value.strip_prefix('"') {
        // A quoted string ends at its closing quote; a comment may follow.
        Some(rest) => &rest[..rest.find('"')?],
        None => value.split_whitespace().next()?,
    };
    Color::from_hex(value)
}

/// Watches for theme changes. Its fd goes in the event loop's poll set.
pub struct Watch {
    fd: OwnedFd,
}

impl Watch {
    /// Starts watching the theme source's `current` directory. `None` when
    /// there's no theme to follow or inotify isn't available.
    pub fn new() -> Option<Self> {
        let dir = current_dir()?;
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).ok()?;
        let flags = WatchFlags::MOVED_TO | WatchFlags::CREATE | WatchFlags::CLOSE_WRITE;
        inotify::add_watch(&fd, &dir, flags).ok()?;
        Some(Self { fd })
    }

    pub fn fd(&self) -> impl AsFd + '_ {
        self.fd.as_fd()
    }

    /// Drains the pending events and says whether any touched the theme.
    /// Call when the fd is readable.
    pub fn changed(&self) -> bool {
        let mut buf = [std::mem::MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut changed = false;
        loop {
            match reader.next() {
                Ok(event) => {
                    if event.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                        changed = true;
                    }
                    if let Some(name) = event.file_name()
                        && is_theme_entry(OsStr::from_bytes(name.to_bytes()))
                    {
                        changed = true;
                    }
                }
                Err(Errno::AGAIN | Errno::INTR) => return changed,
                Err(_) => return changed,
            }
        }
    }
}

/// The entries in `current/` whose change means a new theme.
fn is_theme_entry(name: &OsStr) -> bool {
    Path::new(name) == Path::new("theme")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARNISHED: &str = r##"
# Tarnished: a Dark Souls theme.
accent = "#5E92C6"
cursor = "#A9C2D6"
foreground = "#C5CED6"
background = "#0B111A"
color4 = "#5482B4"
"##;

    #[test]
    fn reads_an_aliasos_theme() {
        let palette = palette_from(TARNISHED).unwrap();
        assert_eq!(palette.background, Color::rgb(0x0b, 0x11, 0x1a));
        assert_eq!(palette.foreground, Color::rgb(0xc5, 0xce, 0xd6));
        assert_eq!(palette.accent, Color::rgb(0x5e, 0x92, 0xc6));
    }

    #[test]
    fn accent_falls_back_to_blue_then_foreground() {
        let text = "background = \"#000000\"\nforeground = \"#ffffff\"\ncolor4 = \"#0000ff\"\n";
        assert_eq!(palette_from(text).unwrap().accent, Color::rgb(0, 0, 255));
        let text = "background = \"#000000\"\nforeground = \"#ffffff\"\n";
        assert_eq!(
            palette_from(text).unwrap().accent,
            Color::rgb(255, 255, 255)
        );
    }

    #[test]
    fn tolerates_comments_spacing_and_bare_values() {
        let text =
            "background=\"#101010\" # dark\n  foreground =   #eeeeee   # bare\naccent = \"nope\"\n";
        let palette = palette_from(text).unwrap();
        assert_eq!(palette.background, Color::rgb(0x10, 0x10, 0x10));
        assert_eq!(palette.foreground, Color::rgb(0xee, 0xee, 0xee));
        // A bad accent is skipped, so the foreground stands in.
        assert_eq!(palette.accent, palette.foreground);
    }

    #[test]
    fn needs_background_and_foreground() {
        assert!(palette_from("background = \"#000000\"\n").is_none());
        assert!(palette_from("").is_none());
    }

    #[test]
    fn only_the_theme_entry_counts() {
        assert!(is_theme_entry(OsStr::new("theme")));
        assert!(!is_theme_entry(OsStr::new("next-theme")));
        assert!(!is_theme_entry(OsStr::new("theme.name")));
        assert!(!is_theme_entry(OsStr::new("background")));
    }

    #[test]
    fn watch_sees_a_theme_swap() {
        let dir = std::env::temp_dir().join(format!("alias-calc-theme-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("next-theme")).unwrap();
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).unwrap();
        inotify::add_watch(
            &fd,
            &dir,
            WatchFlags::MOVED_TO | WatchFlags::CREATE | WatchFlags::CLOSE_WRITE,
        )
        .unwrap();
        let watch = Watch { fd };
        assert!(!watch.changed(), "nothing yet");

        std::fs::write(dir.join("theme.name"), "x").unwrap();
        assert!(!watch.changed(), "other entries don't count");

        std::fs::rename(dir.join("next-theme"), dir.join("theme")).unwrap();
        assert!(watch.changed(), "the swap counts");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

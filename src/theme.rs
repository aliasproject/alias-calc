//! The live aliasOS theme: colors from `current/theme/colors.toml`, re-read
//! whenever the theme changes.
//!
//! `aliasos-theme-apply` swaps the theme directory in with `mv`, which fires
//! `IN_MOVED_TO` for `theme` on its parent (the same event
//! `aliasos-theme-changed` waits on). The theme directory itself briefly
//! stops existing during the swap, so the parent is what's watched. Omarchy
//! swaps a `theme` symlink (`IN_CREATE` or `IN_MOVED_TO`) in
//! `~/.config/omarchy/current`; the same watch covers it.

use std::ffi::OsStr;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

use crate::render::Color;
use crate::ui::Palette;

/// Where themes live, in order: the `current` directories that hold
/// `theme/`. Both paths are fixed under `$HOME`, as `aliasos-theme-apply` and
/// Omarchy's scripts hard-code them (neither follows `XDG_STATE_HOME` or
/// `XDG_CONFIG_HOME`).
fn sources() -> Vec<PathBuf> {
    // An empty HOME would make these relative to the working directory.
    let Some(home) = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
    else {
        return Vec::new();
    };
    vec![
        home.join(".local/state/aliasos/current"),
        home.join(".config/omarchy/current"),
    ]
}

/// The palette of the current theme: aliasOS's, or Omarchy's when aliasOS
/// has none or it can't be read. None when neither gives one.
pub fn load() -> Option<Palette> {
    load_from(&sources())
}

fn load_from(sources: &[PathBuf]) -> Option<Palette> {
    sources.iter().find_map(|dir| {
        let text = std::fs::read_to_string(dir.join("theme/colors.toml")).ok()?;
        palette_from(&text)
    })
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
        // The colors are top-level keys; a [table] after them isn't ours.
        if line.trim_start().starts_with('[') {
            break;
        }
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

/// `"#rrggbb"`, in double, single or no quotes, with an optional trailing
/// `# comment`.
fn color_value(value: &str) -> Option<Color> {
    let value = value.trim();
    let value = match value.chars().next() {
        // A quoted string ends at its closing quote; a comment may follow.
        Some(quote @ ('"' | '\'')) => {
            let rest = &value[1..];
            &rest[..rest.find(quote)?]
        }
        _ => value.split_whitespace().next()?,
    };
    Color::from_hex(value)
}

/// Watches for theme changes. Its fd goes in the event loop's poll set.
pub struct Watch {
    fd: OwnedFd,
    /// The watch on the aliasOS config folder, for the text size.
    text_wd: Option<i32>,
}

/// What a batch of watch events changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub theme: bool,
    /// The aliasOS text size (crate::textsize).
    pub text: bool,
}

impl Watch {
    /// Starts watching every theme source's `current` directory that exists,
    /// so a fallback to Omarchy still sees an aliasOS theme arrive, and the
    /// aliasOS config folder for the text size. `None` when there's nothing
    /// to watch, or inotify isn't available.
    pub fn new() -> Option<Self> {
        Self::on(&sources(), crate::textsize::dir())
    }

    fn on(sources: &[PathBuf], text_dir: Option<PathBuf>) -> Option<Self> {
        let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).ok()?;
        // A theme arrives as an entry named `theme` moved or created in
        // `current/` (a directory or a symlink, never written in place), so
        // file writes beside it (theme.name, theme.generation) aren't asked
        // for and don't wake the loop.
        let flags = WatchFlags::MOVED_TO | WatchFlags::CREATE;
        let watched = sources
            .iter()
            .filter(|dir| inotify::add_watch(&fd, dir.as_path(), flags).is_ok())
            .count();
        // shell.toml is rewritten in place or replaced with mv.
        let text_wd = text_dir.and_then(|dir| {
            inotify::add_watch(
                &fd,
                dir.as_path(),
                WatchFlags::CLOSE_WRITE | WatchFlags::MOVED_TO,
            )
            .ok()
        });
        (watched > 0 || text_wd.is_some()).then_some(Self { fd, text_wd })
    }

    pub fn fd(&self) -> impl AsFd + '_ {
        self.fd.as_fd()
    }

    /// Drains the pending events and says what they changed. Call when the
    /// fd is readable. An error means the watch is broken (its fd would stay
    /// readable and spin the loop), so stop using it.
    pub fn changed(&self) -> Result<Changed, Errno> {
        let mut buf = [std::mem::MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut buf);
        let mut changed = Changed::default();
        loop {
            match reader.next() {
                Ok(event) => {
                    if event.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                        changed = Changed {
                            theme: true,
                            text: true,
                        };
                    }
                    let Some(name) = event.file_name() else {
                        continue;
                    };
                    let name = OsStr::from_bytes(name.to_bytes());
                    if Some(event.wd()) == self.text_wd {
                        if name == crate::textsize::FILE {
                            changed.text = true;
                        }
                    } else if is_theme_entry(name) {
                        changed.theme = true;
                    }
                }
                Err(Errno::AGAIN) => return Ok(changed),
                Err(Errno::INTR) => continue,
                Err(err) => return Err(err),
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

    /// A fresh directory under the system temp dir, removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "alias-calc-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_theme(current: &Path, colors: &str) {
        std::fs::create_dir_all(current.join("theme")).unwrap();
        std::fs::write(current.join("theme/colors.toml"), colors).unwrap();
    }

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
    fn tolerates_comments_spacing_quotes_and_bare_values() {
        let text = "background=\"#101010\" # dark\n  foreground =   #eeeeee   # bare\naccent = 'not a color'\ncolor4 = '#0000ff'\n";
        let palette = palette_from(text).unwrap();
        assert_eq!(palette.background, Color::rgb(0x10, 0x10, 0x10));
        assert_eq!(palette.foreground, Color::rgb(0xee, 0xee, 0xee));
        // A bad accent is skipped, so the single-quoted blue stands in.
        assert_eq!(palette.accent, Color::rgb(0, 0, 255));
    }

    #[test]
    fn ignores_tables_after_the_top_level_keys() {
        let text = format!("{TARNISHED}\n[terminal]\nbackground = \"#ffffff\"\n");
        let palette = palette_from(&text).unwrap();
        assert_eq!(palette.background, Color::rgb(0x0b, 0x11, 0x1a));
    }

    #[test]
    fn needs_background_and_foreground() {
        assert!(palette_from("background = \"#000000\"\n").is_none());
        assert!(palette_from("").is_none());
    }

    #[test]
    fn falls_back_to_omarchy_when_aliasos_has_no_usable_theme() {
        let tmp = TempDir::new("fallback");
        let aliasos = tmp.0.join("aliasos/current");
        let omarchy = tmp.0.join("omarchy/current");
        let sources = [aliasos.clone(), omarchy.clone()];
        write_theme(
            &omarchy,
            "background = \"#222222\"\nforeground = \"#dddddd\"\n",
        );

        // aliasOS's current/ exists but has no theme yet.
        std::fs::create_dir_all(&aliasos).unwrap();
        let palette = load_from(&sources).unwrap();
        assert_eq!(palette.background, Color::rgb(0x22, 0x22, 0x22));

        // A broken aliasOS theme still falls back.
        write_theme(&aliasos, "background = nope\n");
        let palette = load_from(&sources).unwrap();
        assert_eq!(palette.background, Color::rgb(0x22, 0x22, 0x22));

        // A good aliasOS theme wins.
        write_theme(&aliasos, TARNISHED);
        let palette = load_from(&sources).unwrap();
        assert_eq!(palette.background, Color::rgb(0x0b, 0x11, 0x1a));

        // Neither: None, and the caller keeps what it has.
        assert!(load_from(&[tmp.0.join("missing")]).is_none());
    }

    #[test]
    fn only_the_theme_entry_counts() {
        assert!(is_theme_entry(OsStr::new("theme")));
        assert!(!is_theme_entry(OsStr::new("next-theme")));
        assert!(!is_theme_entry(OsStr::new("theme.name")));
        assert!(!is_theme_entry(OsStr::new("background")));
    }

    #[test]
    fn watch_sees_a_theme_swap_in_any_source() {
        let tmp = TempDir::new("watch");
        let aliasos = tmp.0.join("aliasos/current");
        let omarchy = tmp.0.join("omarchy/current");
        std::fs::create_dir_all(aliasos.join("next-theme")).unwrap();
        std::fs::create_dir_all(&omarchy).unwrap();
        let watch = Watch::on(&[aliasos.clone(), omarchy.clone()], None).unwrap();
        assert_eq!(watch.changed().map(|c| c.theme), Ok(false), "nothing yet");

        std::fs::write(aliasos.join("theme.name"), "x").unwrap();
        assert_eq!(
            watch.changed().map(|c| c.theme),
            Ok(false),
            "other entries don't count"
        );

        std::fs::rename(aliasos.join("next-theme"), aliasos.join("theme")).unwrap();
        assert_eq!(
            watch.changed().map(|c| c.theme),
            Ok(true),
            "aliasOS's mv counts"
        );

        std::os::unix::fs::symlink(&aliasos, omarchy.join("theme")).unwrap();
        assert_eq!(
            watch.changed().map(|c| c.theme),
            Ok(true),
            "Omarchy's symlink counts"
        );
    }

    #[test]
    fn watch_sees_the_text_size_change() {
        let tmp = TempDir::new("textwatch");
        let config = tmp.0.join("aliasos");
        std::fs::create_dir_all(&config).unwrap();
        let watch = Watch::on(&[], Some(config.clone())).unwrap();
        std::fs::write(config.join("other.toml"), "x").unwrap();
        assert_eq!(
            watch.changed(),
            Ok(Changed::default()),
            "other files don't count"
        );
        std::fs::write(config.join("shell.toml"), "[font]\nbase-size = 18\n").unwrap();
        assert_eq!(
            watch.changed(),
            Ok(Changed {
                theme: false,
                text: true
            })
        );
    }

    #[test]
    fn no_watch_without_a_source() {
        let tmp = TempDir::new("nowatch");
        assert!(Watch::on(&[tmp.0.join("missing")], None).is_none());
    }
}

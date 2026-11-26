//! Copy and paste: which text formats to offer and accept, and reading a
//! pasted number without blocking the event loop.
//!
//! A paste is read through a pipe the source app writes to. That app may be
//! this one (pasting what it just copied), so the pipe is never read in one
//! blocking go: it sits in the event loop's poll set and is drained as data
//! arrives, while the loop keeps answering the compositor.

use std::io::{ErrorKind, Read};
use std::os::fd::AsFd;

use smithay_client_toolkit::data_device_manager::ReadPipe;

/// Formats offered when copying, most specific first.
pub const OFFERED: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];

/// The best text format among those an offer has.
pub fn pick_format(available: &[String]) -> Option<String> {
    OFFERED
        .iter()
        .find(|want| available.iter().any(|have| have == *want))
        .map(|s| s.to_string())
}

/// A number is short. Anything longer than this isn't one, and reading
/// stops there.
const MAX_PASTE: usize = 256;

/// A paste being read.
pub struct Paste {
    pipe: ReadPipe,
    text: Vec<u8>,
}

/// What reading a paste came to.
pub enum Progress {
    /// More may come.
    Pending,
    /// The source closed the pipe: here's the text.
    Done(String),
    /// It failed, or was too long to be a number.
    Dropped,
}

impl Paste {
    /// Takes the pipe and makes it non-blocking.
    pub fn new(pipe: ReadPipe) -> Option<Self> {
        rustix::io::ioctl_fionbio(&pipe, true).ok()?;
        Some(Self {
            pipe,
            text: Vec::new(),
        })
    }

    pub fn fd(&self) -> impl AsFd + '_ {
        self.pipe.as_fd()
    }

    /// Reads what's there. Call when the pipe is readable.
    pub fn read(&mut self) -> Progress {
        let mut buf = [0u8; 256];
        loop {
            match self.pipe.read(&mut buf) {
                Ok(0) => {
                    return match String::from_utf8(std::mem::take(&mut self.text)) {
                        Ok(text) => Progress::Done(text),
                        Err(_) => Progress::Dropped,
                    };
                }
                Ok(n) => {
                    self.text.extend_from_slice(&buf[..n]);
                    if self.text.len() > MAX_PASTE {
                        return Progress::Dropped;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => return Progress::Pending,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Progress::Dropped,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn prefers_utf8_text() {
        let have = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            pick_format(&have(&["STRING", "text/plain;charset=utf-8", "text/html"])).as_deref(),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            pick_format(&have(&["UTF8_STRING", "TEXT"])).as_deref(),
            Some("UTF8_STRING")
        );
        assert_eq!(pick_format(&have(&["image/png"])), None);
    }

    fn pipe() -> (ReadPipe, std::fs::File) {
        let (r, w) = rustix::pipe::pipe().unwrap();
        (ReadPipe::from(r), std::fs::File::from(w))
    }

    #[test]
    fn reads_in_pieces_without_blocking() {
        let (r, mut w) = pipe();
        let mut paste = Paste::new(r).unwrap();
        assert!(
            matches!(paste.read(), Progress::Pending),
            "nothing written yet"
        );
        w.write_all(b"1,234.").unwrap();
        assert!(matches!(paste.read(), Progress::Pending));
        w.write_all(b"5").unwrap();
        drop(w);
        match paste.read() {
            Progress::Done(text) => assert_eq!(text, "1,234.5"),
            _ => panic!("expected the text"),
        }
    }

    #[test]
    fn drops_long_or_binary_pastes() {
        let (r, mut w) = pipe();
        let mut paste = Paste::new(r).unwrap();
        w.write_all(&[b'9'; MAX_PASTE + 1]).unwrap();
        assert!(matches!(paste.read(), Progress::Dropped));

        let (r, mut w) = pipe();
        let mut paste = Paste::new(r).unwrap();
        w.write_all(&[0xff, 0xfe]).unwrap();
        drop(w);
        assert!(matches!(paste.read(), Progress::Dropped));
    }
}

//! The one look of every human-readable list and report of the CLI: tables with a header row,
//! cards of one item, reports of checks, and the colors of their words. The commands build
//! [`Table`]s, [`Card`]s and [`Report`]s and render them for an [`Out`], so the same word has the
//! same color in every command and the width of the terminal is handled in one place.

mod card;
mod report;
mod table;
mod term;
mod text;

pub use card::Card;
pub use report::{Level, Report};
pub use table::Table;
pub use text::{chars, newest_version, shorten, version_tone};

/// What a word means; each tone has one color everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Ordinary text, no color.
    #[default]
    Plain,
    /// Good: ok, online, available, passed.
    Ok,
    /// Needs a look: warn, busy, behind, stale.
    Warn,
    /// Wrong: error, offline, failed.
    Bad,
    /// Secondary text.
    Dim,
    /// A header.
    Head,
}

impl Tone {
    fn sgr(self) -> Option<&'static str> {
        match self {
            Tone::Plain => None,
            Tone::Ok => Some("32"),
            Tone::Warn => Some("33"),
            Tone::Bad => Some("31"),
            Tone::Dim => Some("2"),
            Tone::Head => Some("1"),
        }
    }
}

/// One cell: its text and the tone it is drawn in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Cell {
    /// The text; a line break in it is shown as a space.
    pub text: String,
    /// The tone.
    pub tone: Tone,
}

impl Cell {
    /// A cell of `tone`.
    pub fn new(text: impl Into<String>, tone: Tone) -> Cell {
        Cell {
            text: text.into().replace(['\n', '\r'], " "),
            tone,
        }
    }

    /// Ordinary text.
    pub fn plain(text: impl Into<String>) -> Cell {
        Cell::new(text, Tone::Plain)
    }

    /// Good text.
    pub fn ok(text: impl Into<String>) -> Cell {
        Cell::new(text, Tone::Ok)
    }

    /// Text that needs a look.
    pub fn warn(text: impl Into<String>) -> Cell {
        Cell::new(text, Tone::Warn)
    }

    /// Wrong text.
    pub fn bad(text: impl Into<String>) -> Cell {
        Cell::new(text, Tone::Bad)
    }

    /// Secondary text.
    pub fn dim(text: impl Into<String>) -> Cell {
        Cell::new(text, Tone::Dim)
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Cell {
        Cell::plain(text)
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Cell {
        Cell::plain(text)
    }
}

/// Where the output goes: whether it is colored and how wide it may be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Out {
    color: bool,
    width: Option<usize>,
}

impl Out {
    /// Plain text: no colors, no shortening. What a pipe, a file and `NO_COLOR` get.
    pub fn plain() -> Out {
        Out {
            color: false,
            width: None,
        }
    }

    /// A terminal of `width` columns that shows colors.
    pub fn styled(width: usize) -> Out {
        Out {
            color: true,
            width: Some(width),
        }
    }

    /// The output of this process: styled only when stdout is a terminal, `NO_COLOR` is not
    /// set and, on Windows, virtual terminal mode could be turned on; the width is the
    /// terminal's, else `COLUMNS`, else unknown (nothing is shortened).
    pub fn detect() -> Out {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        if no_color || !term::stdout_is_terminal() || !term::enable_color() {
            return Out::plain();
        }
        Out {
            color: true,
            width: term::width(),
        }
    }

    /// The width available, `None` when the text is not shortened.
    pub fn width(&self) -> Option<usize> {
        self.width
    }

    /// Whether colors are drawn.
    pub fn color(&self) -> bool {
        self.color
    }

    /// `text` in `tone`.
    pub fn paint(&self, text: &str, tone: Tone) -> String {
        match (self.color, tone.sgr()) {
            (true, Some(code)) if !text.is_empty() => format!("\x1b[{code}m{text}\x1b[0m"),
            _ => text.to_string(),
        }
    }
}

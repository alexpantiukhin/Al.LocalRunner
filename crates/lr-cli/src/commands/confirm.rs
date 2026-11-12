//! Confirmation of commands that destroy something (`uninstall --purge`, `down --cancel`,
//! `runner remove`, `client remove`, `cancel --mine`): `--yes`, or `yes` typed at a prompt;
//! without a terminal only `--yes` confirms.

use std::io::{BufRead, IsTerminal, Write};

/// Where the prompt goes and where the answer comes from.
pub struct Console<'a> {
    /// Whether a person can answer.
    pub terminal: bool,
    /// The answer is read from here.
    pub input: &'a mut dyn BufRead,
    /// The prompt is written here.
    pub out: &'a mut dyn Write,
}

/// Whether the action may go on: `yes`, or `yes` (any case) typed after `question`. Without a
/// terminal nothing is read and the result is an error `"{action}; confirm with --yes"`.
pub fn confirm(
    action: &str,
    question: &str,
    yes: bool,
    console: &mut Console<'_>,
) -> Result<bool, String> {
    if yes {
        return Ok(true);
    }
    if !console.terminal {
        return Err(format!("{action}; confirm with --yes"));
    }
    let _ = write!(console.out, "{question} type yes: ");
    let _ = console.out.flush();
    let mut line = String::new();
    console
        .input
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().eq_ignore_ascii_case("yes"))
}

/// [`confirm`] that also prints `aborted` when the answer is not `yes`.
pub fn proceed(
    action: &str,
    question: &str,
    yes: bool,
    console: &mut Console<'_>,
) -> Result<bool, String> {
    let ok = confirm(action, question, yes, console)?;
    if !ok {
        let _ = writeln!(console.out, "aborted");
    }
    Ok(ok)
}

/// The standard input and output of the process, for building a [`Console`].
pub struct Stdio {
    input: std::io::StdinLock<'static>,
    out: std::io::Stdout,
}

impl Stdio {
    /// Locks standard input.
    pub fn new() -> Stdio {
        Stdio {
            input: std::io::stdin().lock(),
            out: std::io::stdout(),
        }
    }

    /// A console on them; a terminal when standard input is one.
    pub fn console(&mut self) -> Console<'_> {
        Console {
            terminal: std::io::stdin().is_terminal(),
            input: &mut self.input,
            out: &mut self.out,
        }
    }
}

impl Default for Stdio {
    fn default() -> Stdio {
        Stdio::new()
    }
}

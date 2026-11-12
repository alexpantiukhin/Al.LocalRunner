//! Quoting of arguments for the places the OS reads a command line from a file or a string:
//! Windows command lines, systemd `ExecStart=`, XML (launchd, Task Scheduler) and `~/.profile`.

/// One argument as `CommandLineToArgvW` reads it back: quoted when it has blanks or quotes or
/// is empty; backslashes before a quote and at the end of a quoted argument are doubled.
pub fn windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '"']) {
        return arg.to_string();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// A Windows command line of `args`.
pub fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| windows_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ExecStart=` of systemd: each argument in double quotes, `\` and `"` escaped, `%` (unit
/// specifiers) and `$` (variable expansion) doubled, so every argument stays literal.
pub fn systemd_command_line(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            format!(
                "\"{}\"",
                a.replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('%', "%%")
                    .replace('$', "$$")
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text escaped for XML content and attributes.
pub fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A word in POSIX shell single quotes: nothing inside is expanded.
pub fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

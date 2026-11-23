use super::{Error, rustic};

/// One line per error: the program and status, plus the line of its output that says what
/// went wrong.
pub fn summary(error: &Error) -> String {
    match error {
        Error::Failed {
            program,
            status,
            stderr,
        } => format!("{program} failed ({status}): {}", cause(stderr)),
        Error::Unreadable { program, stderr } => {
            let plain = strip_ansi(stderr);
            format!(
                "{program} could not read every file: {}",
                rustic::first_skipped_file(&plain).map_or_else(|| cause(&plain), str::to_string)
            )
        }
        Error::Pipe { producer, consumer } => {
            format!("{}; {}", summary(producer), summary(consumer))
        }
        Error::Cleanup {
            source, cleanup, ..
        } => format!(
            "{}; also could not forget the partial snapshots of this run: {}",
            summary(source),
            summary(cleanup)
        ),
        other => cause(&other.to_string()),
    }
}

/// The first meaningful line, unless it is only a warning and an `[ERROR]` line follows.
/// rustic's generic "`rustic_core` experienced an error" header gets the line after
/// `Message:` appended, since that line holds the actual cause.
fn cause(text: &str) -> String {
    let plain = strip_ansi(text);
    let lines: Vec<&str> = plain.lines().map(str::trim).collect();
    let Some(first) = lines.iter().position(|line| !is_noise(line)) else {
        return "(no error output)".to_string();
    };
    let chosen = if lines[first].starts_with("[WARN]") {
        lines[first..]
            .iter()
            .position(|line| line.starts_with("[ERROR]"))
            .map_or(first, |offset| first + offset)
    } else {
        first
    };
    let line = lines[chosen];
    if line.contains("Command error status: stdin-command") {
        return match lines
            .iter()
            .find(|line| !line.starts_with('[') && !is_noise(line))
        {
            Some(command_output) => format!("{line}: {command_output}"),
            None => line.to_string(),
        };
    }
    if !line.contains("`rustic_core` experienced an error") {
        return line.to_string();
    }
    let message = lines[chosen + 1..]
        .iter()
        .skip_while(|line| !line.starts_with("Message:"))
        .skip(1)
        .find(|line| !line.is_empty());
    match message {
        Some(message) => format!("{line} {message}"),
        None => line.to_string(),
    }
}

fn is_noise(line: &str) -> bool {
    const NOISE: [&str; 5] = [
        "Backtrace:",
        "Message:",
        "Caused by:",
        "Some additional details",
        "error: Not all snapshots were generated successfully",
    ];
    line.is_empty()
        || line.to_ascii_lowercase().contains("disabled backtrace")
        || NOISE.iter().any(|noise| line.starts_with(noise))
}

/// Removes CSI sequences such as `ESC[1m` and `ESC[31m`.
fn strip_ansi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        } else {
            plain.push(c);
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;

    fn failed(stderr: &str) -> Error {
        Error::Failed {
            program: "rustic".into(),
            status: ExitStatus::from_raw(1 << 8),
            stderr: stderr.into(),
        }
    }

    const DUMP_FAILED: &str = "fake pg_dump fails\n\
        [ERROR] Command error status: stdin-command:call : exit status: 3\n\
        [ERROR] error backing up -: `rustic_core` experienced an error.\n\
        \n\
        Message:\n\
        Experienced an error while calling an external command.\n\
        \n\
        Backtrace:\n\
        disabled backtrace (set 'RUST_BACKTRACE=\"1\"' environment variable to enable)\n\
        \n\
        \u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[31merror:\u{1b}[0m Not all snapshots were generated successfully!";

    #[test]
    fn keeps_the_first_meaningful_line_of_stderr() {
        assert_eq!(
            summary(&failed(DUMP_FAILED)),
            "rustic failed (exit status: 1): fake pg_dump fails"
        );
        assert_eq!(
            summary(&failed(
                "[ERROR] error backing up /data/uploads: error sanitizing source\n\
                 \u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[31merror:\u{1b}[0m Not all snapshots were generated successfully!"
            )),
            "rustic failed (exit status: 1): [ERROR] error backing up /data/uploads: error sanitizing source"
        );
    }

    #[test]
    fn strips_ansi_codes_and_backtrace_noise_before_the_message() {
        assert_eq!(
            summary(&failed(
                "\n\u{1b}[1mBacktrace:\u{1b}[0m\ndisabled backtrace\n\u{1b}[31m[ERROR] repo locked\u{1b}[0m\n"
            )),
            "rustic failed (exit status: 1): [ERROR] repo locked"
        );
        assert_eq!(
            summary(&failed(
                "\u{1b}[31merror:\u{1b}[0m Not all snapshots were generated successfully!"
            )),
            "rustic failed (exit status: 1): (no error output)"
        );
    }

    #[test]
    fn summarises_unreadable_pipe_and_cleanup_errors() {
        assert_eq!(
            summary(&Error::Unreadable {
                program: "rustic".into(),
                stderr: "[WARN] ignoring error: /data/uploads/sub: Permission denied\nmore".into(),
            }),
            "rustic could not read every file: [WARN] ignoring error: /data/uploads/sub: Permission denied"
        );
        assert_eq!(
            summary(&Error::Pipe {
                producer: Box::new(failed("one\ntwo")),
                consumer: Box::new(failed("three")),
            }),
            "rustic failed (exit status: 1): one; rustic failed (exit status: 1): three"
        );
        assert_eq!(
            summary(&Error::Cleanup {
                repo: "/backup/local".into(),
                source: Box::new(failed(DUMP_FAILED)),
                cleanup: Box::new(failed("[ERROR] repo locked")),
            }),
            "rustic failed (exit status: 1): fake pg_dump fails; also could not forget the \
             partial snapshots of this run: rustic failed (exit status: 1): [ERROR] repo locked"
        );
        assert_eq!(
            summary(&Error::NoSnapshot {
                tags: "db".into(),
                repo: "/backup/local".into(),
            }),
            "no snapshot tagged db in /backup/local"
        );
    }

    const NO_CONFIG: &str = "\u{1b}[0m\u{1b}[0m\u{1b}[1m\u{1b}[31merror:\u{1b}[0m `rustic_core` experienced an error related to `the configuration`.\n\
        \n\
        Message:\n\
        No repository config file found for `local:/backup/local`. Please check the repository.\n\
        \n\
        \n\
        Some additional details ...\n\
        \n\
        Backtrace:\n\
        disabled backtrace (set 'RUST_BACKTRACE=\"1\"' environment variable to enable)";

    #[test]
    fn adds_the_message_to_rustics_generic_header() {
        assert_eq!(
            summary(&failed(NO_CONFIG)),
            "rustic failed (exit status: 1): error: `rustic_core` experienced an error related to \
             `the configuration`. No repository config file found for `local:/backup/local`. \
             Please check the repository."
        );
    }

    #[test]
    fn prefers_an_error_line_over_earlier_warnings() {
        assert_eq!(
            summary(&failed(
                "[WARN] unrelated: cache dir not found\n[ERROR] the real cause: repo locked"
            )),
            "rustic failed (exit status: 1): [ERROR] the real cause: repo locked"
        );
        assert_eq!(
            summary(&failed("[WARN] unrelated: cache dir not found\n")),
            "rustic failed (exit status: 1): [WARN] unrelated: cache dir not found"
        );
    }

    #[test]
    fn failed_stdin_command_keeps_its_own_message_in_any_order() {
        let linux_order = "[ERROR] Command error status: stdin-command:call : exit status: 3\n\
            fake pg_dump fails\n\
            [ERROR] error backing up -: `rustic_core` experienced an error.\n";
        assert_eq!(
            summary(&failed(linux_order)),
            "rustic failed (exit status: 1): [ERROR] Command error status: stdin-command:call : \
             exit status: 3: fake pg_dump fails"
        );
    }

    #[test]
    fn unreadable_names_the_skipped_file_warning() {
        assert_eq!(
            summary(&Error::Unreadable {
                program: "rustic".into(),
                stderr: "[WARN] some other warning\n\
                         \u{1b}[33m[WARN] ignoring error: /data/uploads/x: Permission denied\u{1b}[0m"
                    .into(),
            }),
            "rustic could not read every file: [WARN] ignoring error: /data/uploads/x: Permission denied"
        );
    }
}

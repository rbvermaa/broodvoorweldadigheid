use super::Error;

/// One line per error: the program and status, plus the first line of its output that says
/// what went wrong.
pub fn summary(error: &Error) -> String {
    match error {
        Error::Failed {
            program,
            status,
            stderr,
        } => format!(
            "{program} failed ({status}): {}",
            first_meaningful_line(stderr)
        ),
        Error::Unreadable { program, stderr } => format!(
            "{program} could not read every file: {}",
            first_meaningful_line(stderr)
        ),
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
        other => first_meaningful_line(&other.to_string()),
    }
}

fn first_meaningful_line(text: &str) -> String {
    strip_ansi(text)
        .lines()
        .map(str::trim)
        .find(|line| !is_noise(line))
        .unwrap_or("(no error output)")
        .to_string()
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
}

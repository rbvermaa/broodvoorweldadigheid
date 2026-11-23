use std::io::Write;
use std::process::{Child, Command, Output, Stdio};

use super::Error;

fn program(command: &Command) -> String {
    command.get_program().to_string_lossy().into_owned()
}

fn spawn(command: &mut Command) -> Result<Child, Error> {
    command.spawn().map_err(|source| Error::Spawn {
        program: program(command),
        source,
    })
}

fn checked(command: &Command, output: Output) -> Result<Vec<u8>, Error> {
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::Failed {
            program: program(command),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

fn wait(command: &Command, child: Child) -> Result<Vec<u8>, Error> {
    let output = child.wait_with_output().map_err(|source| Error::Spawn {
        program: program(command),
        source,
    })?;
    checked(command, output)
}

pub fn run(mut command: Command) -> Result<Vec<u8>, Error> {
    let child = spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    wait(&command, child)
}

/// Like `run`, but returns what the command wrote to stderr; its stdout is discarded.
pub fn run_stderr(mut command: Command) -> Result<String, Error> {
    let child = spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )?;
    let output = child.wait_with_output().map_err(|source| Error::Spawn {
        program: program(&command),
        source,
    })?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    checked(&command, output)?;
    Ok(stderr)
}

pub fn run_with_input(mut command: Command, input: &[u8]) -> Result<Vec<u8>, Error> {
    let mut child = spawn(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).map_err(|source| Error::Spawn {
            program: program(&command),
            source,
        })?;
    }
    wait(&command, child)
}

/// Streams the producer's stdout into the consumer's stdin; both must succeed.
pub fn pipe(mut producer: Command, mut consumer: Command) -> Result<(), Error> {
    let mut source = spawn(producer.stdout(Stdio::piped()).stderr(Stdio::piped()))?;
    let stream = source.stdout.take().map_or_else(Stdio::null, Stdio::from);
    let sink = spawn(
        consumer
            .stdin(stream)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    // The Command keeps its copy of the pipe's read end; while it is open, the producer
    // never sees a broken pipe if the consumer exits early.
    consumer.stdin(Stdio::null());
    let (produced, consumed) = std::thread::scope(|scope| {
        let consumed = scope.spawn(|| sink.and_then(|child| wait(&consumer, child)));
        let produced = wait(&producer, source);
        (produced, consumed.join())
    });
    let consumed = consumed.unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    match (produced, consumed) {
        (Ok(_), Ok(_)) => Ok(()),
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => Err(error),
        (Err(producer), Err(consumer)) => Err(Error::Pipe {
            producer: Box::new(producer),
            consumer: Box::new(consumer),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        command
    }

    #[test]
    fn run_returns_stdout() {
        assert_eq!(run(command("echo", &["hello"])).expect("echo"), b"hello\n");
    }

    #[test]
    fn run_reports_exit_status_and_stderr() {
        let error = run(command("cat", &["/no/such/file"])).expect_err("cat fails");
        let message = error.to_string();
        assert!(
            message.starts_with("cat failed (exit status: 1): "),
            "{message}"
        );
        assert!(message.contains("/no/such/file"), "{message}");
    }

    #[test]
    fn run_reports_missing_program() {
        let error = run(command("no-such-program-for-tests", &[])).expect_err("missing");
        assert!(
            error
                .to_string()
                .starts_with("could not start no-such-program-for-tests: "),
            "{error}"
        );
    }

    #[test]
    fn run_stderr_returns_stderr_of_a_successful_command() {
        assert_eq!(
            run_stderr(command("sh", &["-c", "echo out; echo warning >&2"])).expect("sh"),
            "warning"
        );
        let error =
            run_stderr(command("sh", &["-c", "echo broken >&2; exit 2"])).expect_err("sh fails");
        assert_eq!(error.to_string(), "sh failed (exit status: 2): broken");
    }

    #[test]
    fn run_with_input_feeds_stdin() {
        assert_eq!(
            run_with_input(command("cat", &[]), b"manifest").expect("cat"),
            b"manifest"
        );
    }

    #[test]
    fn pipe_connects_two_commands() {
        pipe(command("echo", &["dump"]), command("grep", &["-q", "dump"])).expect("pipe");
    }

    #[test]
    fn pipe_fails_when_the_consumer_fails() {
        let error = pipe(
            command("echo", &["dump"]),
            command("grep", &["-q", "other"]),
        )
        .expect_err("grep finds nothing");
        assert!(error.to_string().starts_with("grep failed"), "{error}");
    }

    #[test]
    fn pipe_returns_when_the_consumer_exits_before_reading() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = pipe(
                command("head", &["-c", "1048576", "/dev/zero"]),
                command("false", &[]),
            );
            let _ = sender.send(result.map_err(|error| error.to_string()));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("pipe must not hang when the consumer exits early");
        let error = result.expect_err("false fails");
        assert!(error.contains("false failed"), "{error}");
    }

    #[test]
    fn pipe_fails_when_the_producer_fails() {
        let error = pipe(command("cat", &["/no/such/file"]), command("cat", &[]))
            .expect_err("producer fails");
        assert!(error.to_string().starts_with("cat failed"), "{error}");
    }
}

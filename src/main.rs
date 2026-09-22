#![warn(missing_docs)]
#![deny(clippy::undocumented_unsafe_blocks, unsafe_op_in_unsafe_fn)]
//! RunDelta process boundary.
//!
//! This binary parses declarative CLI input, dispatches exactly one use case,
//! renders its already-computed result, and maps uncaught application errors to
//! exit code 2. Capture, storage, comparison, and reliability policy remain in
//! their owning modules.

mod capture;
mod cli;
mod diff;
mod model;
mod parse;
mod report;
mod store;

use clap::Parser;

fn run() -> anyhow::Result<i32> {
    let args = cli::Cli::parse();
    let root = store::root(args.storage)?;
    match args.command {
        cli::Action::Record {
            name,
            command,
            environment,
            max_duration_ms,
            max_trace_bytes,
            terminate_grace_ms,
        } => {
            let limits = capture::CaptureLimits::from_millis(
                max_duration_ms,
                max_trace_bytes,
                terminate_grace_ms,
            );
            let result = capture::record(&root, &name, &command, &environment, limits)?;
            record_output(&mut std::io::stderr().lock(), &result)
        }
        cli::Action::List { json } => {
            let records = store::records(&root)?;
            let text = if json {
                report::list_json(&records)?
            } else {
                report::list_text(&records)
            };
            output(&text, 0)
        }
        cli::Action::Show { name_or_id, json } => {
            let r = store::load(&root, &name_or_id)?;
            let text = if json {
                serde_json::to_string_pretty(&r)?
            } else {
                report::show(&r)
            };
            output(&text, 0)
        }
        cli::Action::Delete { name_or_id } => {
            let id = store::delete(&root, &name_or_id)?;
            output(&format!("Deleted {id}"), 0)
        }
        cli::Action::Diff { left, right, json } => {
            let r = diff::evaluate(
                &left,
                &right,
                store::load_outcome(&root, &left),
                store::load_outcome(&root, &right),
            );
            let text = if json {
                report::json(&r)?
            } else {
                report::text(&r)
            };
            output(&text, r.exit_code)
        }
    }
}
fn output(text: &str, code: i32) -> anyhow::Result<i32> {
    write_output(&mut std::io::stdout().lock(), text, code, "stdout")
}
fn record_output(
    out: &mut impl std::io::Write,
    result: &model::RecordResult,
) -> anyhow::Result<i32> {
    use anyhow::Context;
    write_output(out, &report::record(result), result.cli_exit, "stderr").with_context(|| {
        format!(
            "record {} was committed before its summary output failed",
            result.id
        )
    })
}
fn write_output(
    out: &mut impl std::io::Write,
    text: &str,
    code: i32,
    stream: &str,
) -> anyhow::Result<i32> {
    use anyhow::Context;
    let result = out
        .write_all(text.as_bytes())
        .and_then(|()| {
            if text.ends_with('\n') {
                Ok(())
            } else {
                out.write_all(b"\n")
            }
        })
        .and_then(|()| out.flush());
    match result {
        Ok(()) => Ok(code),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(code),
        Err(e) => Err(e).with_context(|| format!("{stream} output I/O failure")),
    }
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "rundelta: {e:#}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    use crate::model::{CaptureCompleteness, CaptureState, RecordResult};
    use std::io::{self, Write};
    struct Sink {
        fail_flush: bool,
        kind: io::ErrorKind,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_flush {
                Ok(bytes.len().min(2))
            } else {
                Err(self.kind.into())
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(self.kind.into())
        }
    }

    fn finalized_record(code: i32) -> RecordResult {
        RecordResult {
            id: "abc-123".into(),
            name: "test".into(),
            state: CaptureState::Completed,
            completeness: CaptureCompleteness::CompleteForSupportedEvents,
            event_count: 1,
            warnings: vec![],
            evidence_dir: "/records/runs/abc-123".into(),
            cli_exit: code,
        }
    }

    #[test]
    fn broken_pipe_preserves_computed_status_including_errors() {
        for code in [0, 1, 2, 3, 125, 130, 144] {
            for fail_flush in [false, true] {
                assert_eq!(
                    write_output(
                        &mut Sink {
                            fail_flush,
                            kind: io::ErrorKind::BrokenPipe
                        },
                        "report",
                        code,
                        "stdout",
                    )
                    .unwrap(),
                    code
                );
            }
        }
    }
    #[test]
    fn other_write_and_flush_errors_retain_context() {
        for fail_flush in [false, true] {
            let e = write_output(
                &mut Sink {
                    fail_flush,
                    kind: io::ErrorKind::PermissionDenied,
                },
                "report",
                0,
                "stdout",
            )
            .unwrap_err();
            assert!(e.to_string().contains("stdout"));
            assert_eq!(
                e.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let mut bytes = vec![];
        write_output(&mut bytes, "one\n", 0, "stdout").unwrap();
        assert_eq!(bytes, b"one\n");
    }

    #[test]
    fn record_stderr_broken_pipe_preserves_the_business_status() {
        for code in [0, 125, 130, 144] {
            for fail_flush in [false, true] {
                assert_eq!(
                    record_output(
                        &mut Sink {
                            fail_flush,
                            kind: io::ErrorKind::BrokenPipe,
                        },
                        &finalized_record(code),
                    )
                    .unwrap(),
                    code
                );
            }
        }
    }

    #[test]
    fn record_stderr_errors_identify_the_committed_record() {
        for fail_flush in [false, true] {
            let error = record_output(
                &mut Sink {
                    fail_flush,
                    kind: io::ErrorKind::PermissionDenied,
                },
                &finalized_record(0),
            )
            .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("abc-123"), "{message}");
            assert!(message.contains("committed"), "{message}");
            assert!(message.contains("stderr output I/O failure"), "{message}");
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }
}

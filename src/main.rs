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

/// Dispatch one request and preserve the use case's computed status.
///
/// Loading is composed here, but storage failures are passed to `diff::evaluate_report`
/// as facts rather than turned into an empty run. Otherwise a failed read could
/// look like an ordinary disappearance, or even a trusted equal comparison.
fn run() -> anyhow::Result<i32> {
    if let Some(result) = capture::resume_owner()? {
        return record_output(&mut std::io::stderr().lock(), &result);
    }
    let args = cli::Cli::parse();
    let root = store::root(args.storage)?;
    match args.command {
        cli::Action::Record {
            name,
            command,
            environment,
            evidence_contract,
            max_duration_ms,
            max_trace_bytes,
            terminate_grace_ms,
        } => {
            let limits = capture::CaptureLimits::from_millis(
                max_duration_ms,
                max_trace_bytes,
                terminate_grace_ms,
            );
            let contract = if evidence_contract.is_some() {
                model::RecordingEvidenceContract::TraceV1
            } else {
                model::RecordingEvidenceContract::StructuredV3
            };
            let result = capture::record(&root, &name, &command, &environment, limits, contract)?;
            record_output(&mut std::io::stderr().lock(), &result)
        }
        cli::Action::List { json } => {
            let records = store::records(&root)?;
            if json {
                write_json_output(&mut std::io::stdout().lock(), 0, |out| {
                    report::write_list_json(&records, out)
                })
            } else {
                output(&report::list_text(&records), 0)
            }
        }
        cli::Action::Show { name_or_id, json } => {
            let r = store::load(&root, &name_or_id)?;
            if json {
                write_json_output(&mut std::io::stdout().lock(), 0, |out| {
                    report::write_show_json(&r, out)
                })
            } else {
                output(&report::show(&r), 0)
            }
        }
        cli::Action::Delete { name_or_id } => {
            let id = store::delete(&root, &name_or_id)?;
            output(&format!("Deleted {id}"), 0)
        }
        cli::Action::Deleted { command } => match command {
            cli::DeletedAction::List { json } => {
                let records = store::deletion::list(&root)?;
                if json {
                    write_json_output(&mut std::io::stdout().lock(), 0, |out| {
                        serde_json::to_writer_pretty(out, &records)
                    })
                } else {
                    let text = report::deleted_text(&records);
                    output(&text, 0)
                }
            }
            cli::DeletedAction::Restore { ticket } => {
                let id = store::deletion::restore(&root, &ticket)?;
                output(&format!("Restored {id}"), 0)
            }
            cli::DeletedAction::Purge { ticket } => {
                let id = store::deletion::purge(&root, &ticket)?;
                output(&format!("Purged {id}"), 0)
            }
        },
        cli::Action::Diff {
            left,
            right,
            json,
            report_version,
        } => {
            let inputs = (
                store::load_outcome(&root, &left),
                store::load_outcome(&root, &right),
            );
            let format = match report_version {
                None => diff::ReportFormat::Automatic,
                Some(cli::DiffReportVersion::TraceV5) => diff::ReportFormat::TraceV5,
                Some(cli::DiffReportVersion::OutcomeV6) => diff::ReportFormat::OutcomeV6,
            };
            let r = diff::evaluate_report(&left, &right, inputs.0, inputs.1, format);
            if json {
                output_json(&r)
            } else {
                output(&report::text(&r), r.exit_code)
            }
        }
    }
}
fn output(text: &str, code: i32) -> anyhow::Result<i32> {
    write_output(&mut std::io::stdout().lock(), text, code, "stdout")
}

/// Keep stdout ownership and output-error policy outside the presentation layer.
fn output_json(report: &diff::Report) -> anyhow::Result<i32> {
    write_json_output(&mut std::io::stdout().lock(), report.exit_code, |out| {
        report::write_json(report, out)
    })
}

/// Stream one JSON document, then explicitly terminate and flush its output.
///
/// A fixed buffer amortizes the serializer's small writes without retaining the
/// whole document. BrokenPipe preserves the already-computed status even during
/// serialization; other I/O failures retain their original cause, and non-I/O
/// serialization failures remain tool errors. A failed stream may contain a
/// prefix, so every return dismantles the buffer instead of allowing Drop to
/// retry pending bytes after the error has already been classified.
fn write_json_output<W: std::io::Write>(
    out: &mut W,
    code: i32,
    render: impl FnOnce(&mut std::io::BufWriter<&mut W>) -> serde_json::Result<()>,
) -> anyhow::Result<i32> {
    use anyhow::Context;
    let mut buffered = std::io::BufWriter::with_capacity(8 * 1024, out);
    let result = match render(&mut buffered) {
        Ok(()) => write_output(&mut buffered, "", code, "stdout"),
        Err(error) if error.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) => Ok(code),
        Err(error) if error.is_io() => {
            let error: std::io::Error = error.into();
            Err(error).context("stdout output I/O failure")
        }
        Err(error) => Err(error).context("stdout JSON serialization failure"),
    };
    // into_parts never flushes. Discarding uncommitted bytes is intentional on
    // failure; successful output was already explicitly flushed above.
    let _ = buffered.into_parts();
    result
}
fn record_output(
    out: &mut impl std::io::Write,
    result: &model::RecordResult,
) -> anyhow::Result<i32> {
    use anyhow::Context;
    // 记录提交先于摘要输出，输出失败不会撤销已保存事实。对返回的非 BrokenPipe
    // I/O 错误附加已提交 ID，供后续检查；诊断本身能否交付仍取决于 stderr。
    write_output(out, &report::record(result), result.cli_exit, "stderr").with_context(|| {
        format!(
            "record {} was committed before its summary output failed",
            result.id
        )
    })
}
/// Write a presentation without replacing its already-computed business status.
///
/// BrokenPipe normally means a downstream reader intentionally stopped reading;
/// it does not change the recorded/comparison facts. Other write or flush errors
/// remain application errors and are translated to status 2 by `main`.
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
            // Diagnostic delivery is best effort. A second stderr failure is
            // ignored rather than replacing the original error or changing 2.
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
    fn streamed_list_uses_the_existing_stdout_newline_and_error_policy() {
        let mut out = Vec::new();
        assert_eq!(
            write_json_output(&mut out, 0, |out| report::write_list_json(&[], out)).unwrap(),
            0
        );
        assert_eq!(out, b"[]\n");
        for detail in [String::new(), "large damaged-row diagnostic ".repeat(2048)] {
            let rows = [model::RecordSummary::Unreadable {
                id: None,
                name: None,
                directory: "runs/damaged".into(),
                storage_integrity: model::StorageIntegrity {
                    status: "unavailable".into(),
                    detail,
                },
            }];
            for fail_flush in [false, true] {
                for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::Other] {
                    let result = write_json_output(&mut Sink { fail_flush, kind }, 0, |out| {
                        report::write_list_json(&rows, out)
                    });
                    if kind == io::ErrorKind::BrokenPipe {
                        assert_eq!(result.unwrap(), 0);
                    } else {
                        assert!(
                            format!("{:#}", result.unwrap_err())
                                .contains("stdout output I/O failure")
                        );
                    }
                }
            }
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

    /// Exercise buffer exhaustion, short writes and failure without real stdout.
    struct JsonSink {
        bytes: Vec<u8>,
        max_write: usize,
        interrupt_once: bool,
        write_error: Option<io::ErrorKind>,
        fail_after: Option<usize>,
        flush_error: Option<io::ErrorKind>,
        writes: usize,
        failed_writes: usize,
        flushes: usize,
    }

    impl JsonSink {
        fn new(max_write: usize) -> Self {
            Self {
                bytes: vec![],
                max_write,
                interrupt_once: false,
                write_error: None,
                fail_after: None,
                flush_error: None,
                writes: 0,
                failed_writes: 0,
                flushes: 0,
            }
        }
    }

    impl Write for JsonSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if std::mem::take(&mut self.interrupt_once) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if let Some(kind) = self.write_error
                && self
                    .fail_after
                    .is_none_or(|limit| self.bytes.len() >= limit)
            {
                self.failed_writes += 1;
                return Err(kind.into());
            }
            let count = bytes.len().min(self.max_write).min(
                self.fail_after
                    .map_or(usize::MAX, |limit| limit.saturating_sub(self.bytes.len())),
            );
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.flush_error.map_or(Ok(()), |kind| Err(kind.into()))
        }
    }

    #[test]
    fn streamed_show_json_preserves_run_shapes_and_partial_output_policy() {
        let legacy: crate::model::Run = serde_json::from_str(include_str!(
            "../tests/fixtures/contract_baseline/golden/show.json"
        ))
        .unwrap();
        let trace = crate::model::trace_contract_tests::run_fixture();
        for mut run in [legacy, trace] {
            assert!(matches!(run.metadata.schema_version, 3 | 4));
            // At least 32 KiB of source text with escapes/non-ASCII; never modify saved
            // fixtures or introduce a new JSON compatibility/presentation view.
            run.metadata.command.push("\"\\\n中文".repeat(4096));
            let expected = serde_json::to_string_pretty(&run).unwrap() + "\n";
            assert!(expected.len() > 32 * 1024);
            let mut out = JsonSink::new(37);
            out.interrupt_once = true;
            assert_eq!(
                write_json_output(&mut out, 0, |out| report::write_show_json(&run, out)).unwrap(),
                0
            );
            assert_eq!(out.bytes, expected.as_bytes());
            assert_eq!(out.flushes, 1);

            for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::PermissionDenied] {
                let mut out = JsonSink::new(37);
                out.write_error = Some(kind);
                out.fail_after = Some(123);
                let result =
                    write_json_output(&mut out, 0, |out| report::write_show_json(&run, out));
                if kind == io::ErrorKind::BrokenPipe {
                    assert_eq!(result.unwrap(), 0);
                } else {
                    let error = result.unwrap_err();
                    assert!(error.to_string().contains("stdout output I/O failure"));
                    assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), kind);
                    // main's existing uncaught-Err boundary maps this to process exit 2;
                    // this helper preserves the cause rather than computing 2 itself.
                }
                assert_eq!(out.bytes, expected.as_bytes()[..123]);
                assert_eq!(out.failed_writes, 1);
                assert_eq!(out.flushes, 0);
            }
        }
    }

    #[test]
    fn streamed_json_keeps_exact_bytes_one_newline_and_status_after_short_writes() {
        for length in [0, 100, 32 * 1024] {
            // Escaping and non-ASCII content must use the same pretty serializer.
            let value = vec!["\"\\\n中文".to_owned(), "x".repeat(length)];
            let expected = serde_json::to_string_pretty(&value).unwrap() + "\n";
            for code in 0..=3 {
                let mut out = JsonSink::new(37);
                out.interrupt_once = true;
                assert_eq!(
                    write_json_output(&mut out, code, |out| {
                        serde_json::to_writer_pretty(out, &value)
                    })
                    .unwrap(),
                    code
                );
                assert_eq!(out.bytes, expected.as_bytes());
                assert_eq!(out.flushes, 1);
            }
        }
    }

    #[test]
    fn streamed_json_write_and_flush_errors_preserve_the_four_way_contract() {
        for length in [100, 32 * 1024] {
            let value = "x".repeat(length);
            for code in 0..=3 {
                for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::PermissionDenied] {
                    for at_flush in [false, true] {
                        let mut out = JsonSink::new(usize::MAX);
                        if at_flush {
                            out.flush_error = Some(kind);
                        } else {
                            out.write_error = Some(kind);
                        }
                        let result = write_json_output(&mut out, code, |out| {
                            serde_json::to_writer_pretty(out, &value)
                        });
                        if kind == io::ErrorKind::BrokenPipe {
                            assert_eq!(result.unwrap(), code);
                        } else {
                            let error = result.unwrap_err();
                            assert!(error.to_string().contains("stdout output I/O failure"));
                            assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), kind);
                        }
                        if at_flush {
                            assert_eq!(out.flushes, 1);
                        } else {
                            // No implicit Drop flush may retry the failed write.
                            assert_eq!(out.writes, 1);
                            assert_eq!(out.flushes, 0);
                        }
                    }
                }
            }
        }
        let error = write_json_output(&mut JsonSink::new(0), 1, |out| {
            serde_json::to_writer_pretty(out, &"value")
        })
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::WriteZero
        );
    }

    /// A serializer can fail after writing a valid prefix, independent of I/O.
    struct RejectedJson {
        prefix_length: usize,
    }

    #[test]
    fn streamed_json_failure_after_partial_delivery_does_not_retry_buffered_bytes() {
        // Many small elements fill the buffer, so the failure leaves a pending
        // suffix inside BufWriter rather than failing only a direct large write.
        let value = vec!["x".repeat(50); 512];
        let expected = serde_json::to_string_pretty(&value).unwrap();
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::PermissionDenied] {
            for code in 0..=3 {
                let mut out = JsonSink::new(37);
                out.write_error = Some(kind);
                out.fail_after = Some(123);
                let result = write_json_output(&mut out, code, |out| {
                    serde_json::to_writer_pretty(out, &value)
                });
                if kind == io::ErrorKind::BrokenPipe {
                    assert_eq!(result.unwrap(), code);
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(error.downcast_ref::<io::Error>().unwrap().kind(), kind);
                }
                assert_eq!(out.bytes, expected.as_bytes()[..123]);
                assert_eq!(out.failed_writes, 1);
                assert_eq!(out.flushes, 0);
            }
        }
    }

    impl serde::Serialize for RejectedJson {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::{Error, SerializeSeq};
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&"x".repeat(self.prefix_length))?;
            Err(S::Error::custom("deliberate serialization fault"))
        }
    }

    #[test]
    fn streamed_json_serialization_errors_discard_pending_bytes_without_flush() {
        for prefix_length in [0, 32 * 1024] {
            let value = RejectedJson { prefix_length };
            let mut out = JsonSink::new(usize::MAX);
            let error =
                write_json_output(&mut out, 0, |out| serde_json::to_writer_pretty(out, &value))
                    .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("stdout JSON serialization failure")
            );
            let cause = error.downcast_ref::<serde_json::Error>().unwrap();
            assert!(!cause.is_io());
            assert!(cause.to_string().contains("deliberate serialization fault"));
            assert_eq!(out.flushes, 0);
            if prefix_length == 0 {
                assert!(out.bytes.is_empty());
                assert_eq!(out.writes, 0);
            } else {
                // The opening and long string reach the sink, but its closing
                // quote remains buffered when Serialize returns the fault.
                let expected = "[\n  \"".to_owned() + &"x".repeat(prefix_length);
                assert_eq!(out.bytes, expected.as_bytes());
            }
        }
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

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
        } => capture::record(&root, &name, &command, &environment),
        cli::Action::List { json } => {
            let records = store::records(&root)?;
            let text = if json {
                serde_json::to_string_pretty(&records)?
            } else {
                records
                    .iter()
                    .map(|r| {
                        format!(
                            "{}\t{}\t{}\tcapture={}\tstorage={}\t{}\n",
                            r["id"].as_str().unwrap_or("(no safe ID)"),
                            r["name"].as_str().unwrap_or("(unknown label)"),
                            r["directory"].as_str().unwrap_or(""),
                            r["completeness"].as_str().unwrap_or("unknown"),
                            r["storage_integrity"]["status"]
                                .as_str()
                                .unwrap_or("unavailable"),
                            r["storage_integrity"]["detail"].as_str().unwrap_or(""),
                        )
                    })
                    .collect()
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
                store::load(&root, &left),
                store::load(&root, &right),
            );
            let text = if json {
                serde_json::to_string_pretty(&r)?
            } else {
                report::text(&r)
            };
            output(&text, r.exit_code)
        }
    }
}
fn output(text: &str, code: i32) -> anyhow::Result<i32> {
    write_output(&mut std::io::stdout().lock(), text, code)
}
fn write_output(out: &mut impl std::io::Write, text: &str, code: i32) -> anyhow::Result<i32> {
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
        Err(e) => Err(e).context("stdout output I/O failure"),
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
    #[test]
    fn broken_pipe_preserves_computed_status_including_errors() {
        for code in [0, 1, 2, 3] {
            for fail_flush in [false, true] {
                assert_eq!(
                    write_output(
                        &mut Sink {
                            fail_flush,
                            kind: io::ErrorKind::BrokenPipe
                        },
                        "report",
                        code
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
            )
            .unwrap_err();
            assert!(e.to_string().contains("stdout"));
            assert_eq!(
                e.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let mut bytes = vec![];
        write_output(&mut bytes, "one\n", 0).unwrap();
        assert_eq!(bytes, b"one\n");
    }
}

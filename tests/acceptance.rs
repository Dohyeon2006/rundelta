#[test]
fn gate_control_flow() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/full_gate.py")
        .arg("--self-test")
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux, cc, python3 and strace (or RUNDELTA_STRACE)"]
fn real_capture() {
    let status = std::process::Command::new("python3")
        .arg("examples/run.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux x86_64, cc, python3, pidfds, PTY and strace"]
fn reliability_regressions() {
    let status = std::process::Command::new("python3")
        .arg("tests/reliability.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux, cc, python3, pidfds and strace"]
fn runtime_boundary_regressions() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/runtime_boundary.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux non-root, cc, python3, strace and /dev/full"]
fn storage_cli_regressions() {
    let status = std::process::Command::new("python3")
        .arg("tests/storage_cli.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux x86_64, cc, python3 and strace"]
fn cwd_uncertainty_regressions() {
    let status = std::process::Command::new("python3")
        .arg("tests/cwd_uncertainty.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux, python3 and pidfds; synthetic backend, no real target"]
fn completion_equivalence_regressions() {
    let status = std::process::Command::new("python3")
        .arg("tests/completion_equivalence.py")
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux x86_64, cc, python3 and strace"]
fn fs_context_regressions() {
    let status = std::process::Command::new("python3")
        .arg("tests/fs_context.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

#[test]
#[ignore = "requires Linux, python3, pidfds, strace and a user-owned mount namespace"]
fn mount_namespace_regressions() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/mount_namespace.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("python3 required");
    assert!(status.success());
}

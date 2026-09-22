#[test]
fn public_synthetic_cli_contracts() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/public_cli.py")
        .arg("--synthetic")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("Python 3 required for the independent synthetic CLI tests");
    assert!(status.success(), "synthetic public CLI contracts failed");
}

#[test]
#[ignore = "requires Linux, Python 3, cc, pidfds and a compatible strace"]
fn public_cli_workflow() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/public_cli.py")
        .arg("--real")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("Python 3 required for the public CLI test");
    assert!(status.success(), "public CLI workflow failed");
}

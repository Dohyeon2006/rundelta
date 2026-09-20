#[test]
#[ignore = "requires Linux, Python 3, cc, pidfds and a compatible strace"]
fn public_cli_workflow() {
    let status = std::process::Command::new("python3")
        .arg("-B")
        .arg("tests/public_cli.py")
        .env("RUNDELTA_BIN", env!("CARGO_BIN_EXE_rundelta"))
        .status()
        .expect("Python 3 required for the public CLI test");
    assert!(status.success(), "public CLI workflow failed");
}

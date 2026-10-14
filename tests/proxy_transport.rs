use std::process::Command;

// Run serially: each HTTP case owns a fresh private Carry process and fake-auth
// home. No Python dependencies, model calls, or inherited provider credentials.
#[test]
fn transport_http_regressions() {
    let output = Command::new("python3")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "-B",
            "tests/test_proxy_transport.py",
            "--binary",
            env!("CARGO_BIN_EXE_carry"),
        ])
        .output()
        .expect("Python 3 is required for the model-free transport HTTP fixtures");
    assert!(
        output.status.success(),
        "transport HTTP regressions failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

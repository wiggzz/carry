use std::process::Command;

#[test]
fn keep_lease_primary_round_target_review_release_tracer() {
    let output = Command::new("python3")
        .arg("-B")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/proxy_keep_leases.py"
        ))
        .env("CARRY_TEST_BINARY", env!("CARGO_BIN_EXE_carry"))
        .output()
        .expect("Python must run the loopback keep-lease tracer");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

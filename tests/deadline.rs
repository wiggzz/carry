// Run the model-free deadline seams against Cargo's exact candidate binary.
// Linux is required for SIGUSR1 and the adapter's /proc/subreaper ownership.
#[cfg(target_os = "linux")]
#[test]
fn deadline_usage_and_owned_process_regressions() {
    use std::process::Command;

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for args in [
        vec![
            "-m",
            "unittest",
            "scripts.test_swebench_deadline",
            "scripts.test_swebench_deadline_processes",
            "-v",
        ],
        vec![
            "tests/test_native_deadline.py",
            "--binary",
            env!("CARGO_BIN_EXE_carry"),
            "-v",
        ],
    ] {
        let output = Command::new("python3")
            .current_dir(root)
            .args(args)
            .output()
            .expect("Python 3 is required for the model-free deadline fixtures");
        assert!(
            output.status.success(),
            "deadline fixture failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

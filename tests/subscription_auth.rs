use std::process::Command;

#[test]
fn login_help_offers_browser_and_device_authentication() {
    let output = Command::new(env!("CARGO_BIN_EXE_carry"))
        .args(["login", "--help"])
        .output()
        .expect("run carry login --help");

    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help output");
    assert!(help.contains("Sign in with a ChatGPT subscription"));
    assert!(help.contains("--device-auth"));
}

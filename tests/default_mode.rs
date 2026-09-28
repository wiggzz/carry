use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[test]
fn no_prompt_starts_web_server_without_reading_stdin() {
    let temp = tempfile::tempdir().unwrap();
    let steps = temp.path().join("steps.jsonl");
    std::fs::write(&steps, r#"{"action":{"kind":"finish","answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_carry"))
        .args(["--no-open", "--port", "0", "--scripted-steps"])
        .arg(steps)
        .arg("--session-dir")
        .arg(temp.path().join("session"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("carry web UI: http://127.0.0.1:") {
                let _ = tx.send(());
                break;
            }
        }
    });
    let result = rx.recv_timeout(Duration::from_secs(10));
    let _ = child.kill();
    child.wait().unwrap();
    assert!(
        result.is_ok(),
        "default invocation did not start web mode: {result:?}"
    );
}

#[cfg(unix)]
#[test]
fn web_startup_opens_browser_at_bound_address() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("opened-url");
    let opener = temp.path().join(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    });
    std::fs::write(
        &opener,
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$CAPTURE_URL\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
    let steps = temp.path().join("steps.jsonl");
    std::fs::write(&steps, r#"{"action":{"kind":"finish","answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_carry"))
        .args(["--serve", "--port", "0", "--scripted-steps"])
        .arg(steps)
        .arg("--session-dir")
        .arg(temp.path().join("session"))
        .env(
            "PATH",
            format!(
                "{}:{}",
                temp.path().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("CAPTURE_URL", &capture)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut url = String::new();
    while std::time::Instant::now() < deadline {
        url = std::fs::read_to_string(&capture).unwrap_or_default();
        if !url.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let connected = url
        .strip_prefix("http://")
        .is_some_and(|address| std::net::TcpStream::connect(address).is_ok());
    let _ = child.kill();
    child.wait().unwrap();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "browser was not launched: {url:?}"
    );
    assert!(
        connected,
        "browser URL must use the actual listening port: {url}"
    );
}

#[test]
fn web_session_keeps_log_out_of_terminal() {
    use std::io::{Read, Write};
    let temp = tempfile::tempdir().unwrap();
    let steps = temp.path().join("steps.jsonl");
    std::fs::write(&steps, r#"{"action":{"kind":"finish","answer":"done"},"context":{"protected":[],"removable":[],"remember":[]}}"#).unwrap();
    let stderr_path = temp.path().join("stderr");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let session = temp.path().join("session");
    let mut child = Command::new(env!("CARGO_BIN_EXE_carry"))
        .args(["--no-open", "--port", "0", "--scripted-steps"])
        .arg(&steps)
        .arg("--session-dir")
        .arg(&session)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .unwrap();
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let url = loop {
            let output = std::fs::read_to_string(&stderr_path).unwrap();
            if let Some(url) = output
                .lines()
                .find_map(|line| line.strip_prefix("carry web UI: "))
            {
                break url.to_owned();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "server did not start: {output}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let addr = url.strip_prefix("http://").unwrap();
        let mut socket = std::net::TcpStream::connect(addr).unwrap();
        let body = r#"{"message":"hello"}"#;
        write!(socket, "POST /api/v1/messages HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        socket.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 202"), "{response}");
        let trace_path = session.join("trace.log");
        loop {
            let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
            if trace.contains("done") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "session did not finish: {trace}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let terminal = std::fs::read_to_string(&stderr_path).unwrap();
        assert!(
            !terminal.contains("done") && !terminal.contains("Session started"),
            "web log leaked to terminal: {terminal}"
        );
    }
    let _ = child.kill();
    child.wait().unwrap();
}

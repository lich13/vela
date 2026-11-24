//! The server program itself, run as a service would run it: over a home
//! directory and a socket of its own.
#![cfg(all(unix, not(feature = "desktop")))]
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn vela(home: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vela"));
    command
        .args(arguments)
        .env("HOME", home)
        .env("VELA_SERVER_SOCKET", home.join("rpc.sock"))
        // Each of these would point a client's directory outside `home`.
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("GROK_HOME")
        .env_remove("DSH_HOME")
        .stdin(Stdio::null());
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn status(output: &Output) -> (Option<i32>, String) {
    (output.status.code(), text(&output.stdout))
}

#[test]
fn the_server_answers_until_it_is_told_to_stop_and_leaves_no_socket() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let socket = home.join("rpc.sock");
    let version = format!("vela-server {}\n", env!("CARGO_PKG_VERSION"));

    let asked = vela(home, &["--version"]).output().unwrap();
    assert_eq!(status(&asked), (Some(0), version));
    for arguments in [&[][..], &["serve", "now"], &["desktop"]] {
        let refused = vela(home, arguments).output().unwrap();
        assert_eq!(status(&refused), (Some(2), String::new()));
        assert!(text(&refused.stderr).contains("serve | rpc-stdio | health"));
    }
    for client in ["health", "rpc-stdio"] {
        let alone = vela(home, &[client]).output().unwrap();
        assert_eq!(status(&alone), (Some(1), String::new()), "{client}");
        assert!(text(&alone.stderr).contains("无法连接"), "{client}");
    }

    let mut server = vela(home, &["serve"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !socket.exists() {
        assert!(started.elapsed() < Duration::from_secs(30), "no socket");
        assert!(server.try_wait().unwrap().is_none(), "the server ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    let healthy = vela(home, &["health"]).output().unwrap();
    assert_eq!(
        status(&healthy),
        (Some(0), "ok\n".into()),
        "{}",
        text(&healthy.stderr)
    );
    let second = vela(home, &["serve"]).output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(text(&second.stderr).contains("已有 Vela 服务器"));
    assert!(socket.exists(), "the second server took the socket away");

    // What an SSH session would run: the frames go through untouched.
    let mut relay = vela(home, &["rpc-stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = br#"{"id":5,"command":"system.capabilities"}"#;
    let mut typed = relay.stdin.take().unwrap();
    typed
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    typed.write_all(request).unwrap();
    let mut shown = relay.stdout.take().unwrap();
    let mut length = [0; 4];
    shown.read_exact(&mut length).unwrap();
    let mut answer = vec![0; u32::from_be_bytes(length) as usize];
    shown.read_exact(&mut answer).unwrap();
    let answer: serde_json::Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(answer["id"], 5);
    assert_eq!(answer["result"]["version"], env!("CARGO_PKG_VERSION"));
    let commands = answer["result"]["commands"].as_array().unwrap();
    assert!(commands.iter().any(|name| name == "get_gateway"));
    drop(typed);
    assert_eq!(relay.wait().unwrap().code(), Some(0));

    // SAFETY: the process is this test's own child, which has not been waited for.
    assert_eq!(unsafe { libc::kill(server.id() as i32, libc::SIGTERM) }, 0);
    let ended = server.wait_with_output().unwrap();
    assert_eq!(ended.status.code(), Some(0), "{}", text(&ended.stderr));
    assert!(text(&ended.stderr).contains("正在监听"));
    assert!(!socket.exists(), "the socket outlived the server");
    assert!(home.join(".vela").is_dir(), "the data was kept elsewhere");
}

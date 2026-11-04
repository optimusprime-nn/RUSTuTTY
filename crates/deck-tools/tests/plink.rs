//! The real Plink against a test SSH server on localhost.

#[path = "../../deck-core/tests/support/server.rs"]
#[allow(dead_code)]
mod server;

use russh::keys::Algorithm;
use russh::MethodKind;
use server::{new_key, PASSWORD, USER};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Plink exits with the remote command's exit code, as PuTTY's does. The test
/// server's shell exits with 3 on "exit".
#[tokio::test(flavor = "multi_thread")]
async fn plink_exits_with_the_remote_status() {
    let (port, _seen) = server::start(new_key(Algorithm::Ed25519), vec![MethodKind::Password]).await;
    let mut plink = Command::new(env!("CARGO_BIN_EXE_plink"))
        .args(["-ssh", "-P", &port.to_string(), "-l", USER, "-t", "127.0.0.1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = plink.stdin.take().unwrap();
    let mut stderr = plink.stderr.take().unwrap();
    let prompts = std::thread::spawn(move || {
        // answer each prompt as it appears: "o" connects once, so no key is
        // saved in this computer's registry
        let mut seen = String::new();
        let mut buf = [0u8; 512];
        let mut answered = 0;
        let end = Instant::now() + Duration::from_secs(20);
        while answered < 2 && Instant::now() < end {
            let n = stderr.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if answered == 0 && seen.contains("Trust this host") {
                stdin.write_all(b"o\r").unwrap();
                answered = 1;
            }
            if answered == 1 && seen.contains("password: ") {
                stdin.write_all(format!("{PASSWORD}\r").as_bytes()).unwrap();
                answered = 2;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        stdin.write_all(b"exit\r").unwrap();
        stdin.flush().unwrap();
        let mut rest = String::new();
        let _ = stderr.read_to_string(&mut rest);
        (seen + &rest, stdin)
    });
    let end = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = plink.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > end {
            let _ = plink.kill();
            panic!("plink didn't finish");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let (seen, _stdin) = prompts.join().unwrap();
    let mut out = String::new();
    let _ = plink.stdout.take().unwrap().read_to_string(&mut out);
    assert_eq!(status.code(), Some(3), "prompts: {seen:?} output: {out:?}");
}

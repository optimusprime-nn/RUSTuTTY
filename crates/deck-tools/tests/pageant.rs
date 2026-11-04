//! The real Pageant, holding throwaway keys.

use deck_core::ppk::{self, Ppk};
use russh::keys::agent::client::AgentClient;
use russh::keys::ssh_key::Algorithm;
use russh::keys::PrivateKey;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

fn running() -> bool {
    unsafe { FindWindowW(w!("Pageant"), w!("Pageant")).is_ok() }
}

fn key_file(dir: &std::path::Path, name: &str) -> String {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let (algorithm, public, private) = ppk::blobs_of(&key).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, Ppk::write(&algorithm, name, &public, &private, "", None).unwrap()).unwrap();
    path.to_string_lossy().into_owned()
}

/// The comments of the keys the running Pageant holds, or `None` if it didn't
/// answer.
async fn held() -> Option<Vec<String>> {
    let stream = pageant::wmmessage::PageantStream::new().await.ok()?;
    Some(AgentClient::connect(stream).request_identities().await.ok()?.iter().map(|k| k.comment().to_string()).collect())
}

/// Pageant holds the keys it's given, and a second Pageant hands its keys to
/// the first, as PuTTY's does.
#[tokio::test(flavor = "multi_thread")]
async fn pageant_holds_keys_and_takes_more_from_a_second_one() {
    // never near a Pageant that's in real use
    if running() {
        eprintln!("skipped: a Pageant is already running");
        return;
    }
    let dir = std::env::temp_dir().join(format!("rustutty-pageant-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (one, two) = (key_file(&dir, "first.ppk"), key_file(&dir, "second.ppk"));
    let first = Command::new(env!("CARGO_BIN_EXE_pageant")).arg(&one).env("RUSTUTTY_PAGEANT_NO_TRAY", "1").stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // it stops at the end however the test goes
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let _first = Stop(first);
    let end = Instant::now() + Duration::from_secs(10);
    while !running() || held().await.is_none_or(|k| k.is_empty()) {
        assert!(Instant::now() < end, "Pageant never came up with its key, or never answered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(held().await.unwrap(), ["first.ppk"]);
    let second = Command::new(env!("CARGO_BIN_EXE_pageant")).arg(&two).env("RUSTUTTY_PAGEANT_NO_TRAY", "1").output().unwrap();
    assert!(second.status.success(), "the second one handed its key over and left: {}", String::from_utf8_lossy(&second.stderr));
    let mut keys = held().await.unwrap();
    keys.sort();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(keys, ["first.ppk", "second.ppk"]);
}

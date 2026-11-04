//! Rlogin sessions, after `otherbackends/rlogin.c`.

use crate::config::Target;
use crate::proxy::Conn;
use crate::session::{Command, Event, Io};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn startup(local: &str, remote: &str, term: &str, speed: &str) -> Vec<u8> {
    let mut msg = vec![0];
    for field in [local, remote] {
        msg.extend(field.as_bytes());
        msg.push(0);
    }
    msg.extend(format!("{term}/{speed}").as_bytes());
    msg.push(0);
    msg
}

fn window_size(cols: u32, rows: u32) -> [u8; 12] {
    let (r, c) = (rows as u16, cols as u16);
    let [r1, r0] = r.to_be_bytes();
    let [c1, c0] = c.to_be_bytes();
    [0xff, 0xff, b's', b's', r1, r0, c1, c0, 0, 0, 0, 0]
}

/// How often to look for the server's urgent request for the window size.
const URGENT_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

/// The server asks for the window size with an urgent byte that has 0x80 set,
/// and PuTTY sends it only then, as RFC 1282 says. `urgent` is the socket to
/// read that byte from; through a proxy there's none, and the size goes as
/// soon as the server answers the login instead.
pub(crate) async fn run(target: &Target, mut conn: Conn, urgent: Option<socket2::Socket>, mut io: Io) -> anyhow::Result<String> {
    let s = &target.settings;
    let local = s.s("LocalUserName", "");
    let local = if local.is_empty() { std::env::var("USERNAME").unwrap_or_default() } else { local };
    let remote = match target.user.clone() {
        Some(u) => u,
        None => io.read_line("rlogin username: ", true).await.ok_or_else(|| anyhow::anyhow!("Login cancelled."))?,
    };
    let speed = s.s("TerminalSpeed", "38400,38400");
    let speed = speed.split(',').next().unwrap_or("38400");
    conn.write_all(&startup(&local, &remote, &s.s("TerminalType", "xterm"), speed)).await?;

    io.logged_in();
    let mut first = true;
    let mut buf = [0u8; 8192];
    // whether the server has asked for the window size
    let mut can_size = false;
    let mut look = tokio::time::interval(URGENT_EVERY);
    loop {
        tokio::select! {
            _ = look.tick(), if urgent.is_some() => {
                let mut byte = [std::mem::MaybeUninit::<u8>::uninit()];
                if let Some(Ok(1)) = urgent.as_ref().map(|s| s.recv_out_of_band(&mut byte)) {
                    // SAFETY: recv_out_of_band filled the one byte it said it did
                    let b = unsafe { byte[0].assume_init() };
                    if b & 0x80 != 0 {
                        can_size = true;
                        conn.write_all(&window_size(io.cols, io.rows)).await?;
                    }
                }
            }
            n = conn.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    return Ok("Connection closed by remote host.".into());
                }
                let mut data = &buf[..n];
                if first {
                    // the server's single zero byte says it accepted the login
                    first = false;
                    if data[0] == 0 {
                        data = &data[1..];
                    }
                    // with no way to hear the request, the size goes now
                    if urgent.is_none() {
                        can_size = true;
                        conn.write_all(&window_size(io.cols, io.rows)).await?;
                    }
                }
                if !data.is_empty() {
                    (io.events)(Event::Output(data.to_vec()));
                }
            }
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => return Ok("Session closed.".into()),
                // answers reach here as Input, once the session is ready
                Some(Command::Reply(_) | Command::Special(_) | Command::Reconfigure(_)) => {}
                Some(Command::Input(b)) => conn.write_all(&b).await?,
                Some(Command::Resize { cols, rows }) => {
                    (io.cols, io.rows) = (cols, rows);
                    if can_size {
                        conn.write_all(&window_size(cols, rows)).await?;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_has_rfc1282_shape() {
        assert_eq!(startup("me", "you", "xterm", "38400"), b"\0me\0you\0xterm/38400\0");
    }

    #[test]
    fn window_size_is_big_endian() {
        assert_eq!(window_size(80, 24), [0xff, 0xff, b's', b's', 0, 24, 0, 80, 0, 0, 0, 0]);
    }
}

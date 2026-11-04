//! SSH sessions, on russh.

use crate::algorithms;
use crate::authplugin::{Plugin, Prompts, Step};
use crate::config::Target;
use crate::hostkeys::{self, fingerprint, HostKeyStore, Verdict};
use crate::ppk::Ppk;
use crate::proxy::{self, Conn};
use crate::session::{clean, Command, Event, EventSink, Io};
use crate::settings::{Forward, Settings};
use crate::x11auth;
use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, PrivateKey, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
use russh::{compression, Channel, ChannelMsg, ChannelOpenFailure, ChannelStream, MethodKind};
use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

const MAX_TRIES: usize = 3;

/// Only the older window-message route to Pageant: the named pipe's owner can't
/// be checked through russh, and another account could be listening on it.
async fn connect_pageant() -> anyhow::Result<AgentClient<pageant::wmmessage::PageantStream>> {
    Ok(AgentClient::connect(pageant::wmmessage::PageantStream::new().await?))
}

struct Client {
    host: String,
    port: u16,
    store: Arc<dyn HostKeyStore>,
    io: Arc<Mutex<Io>>,
    events: EventSink,
    /// The key accepted for this connection. A re-key presents it again while
    /// the session holds `io`, so that must be answered without taking the lock.
    accepted: Option<String>,
    manual_keys: Vec<String>,
    banner: bool,
    /// What this session asked for. Anything else the server opens is refused.
    remote_forwards: RemoteForwards,
    agent: bool,
    x11: Option<X11>,
}

impl Client {
    fn say(&self, text: &str) {
        (self.events)(Event::Local(text.replace('\n', "\r\n").into_bytes()));
    }

    fn is_manual(&self, key: &PublicKey, print: &str) -> bool {
        let line = key.to_openssh().unwrap_or_default();
        let blob = line.split_whitespace().nth(1).unwrap_or_default();
        self.manual_keys.iter().any(|k| k == print || k == blob)
    }

    async fn accept(&mut self, key: &PublicKey) -> anyhow::Result<bool> {
        let print = fingerprint(key);
        if self.accepted.as_deref() == Some(print.as_str()) {
            return Ok(true);
        }
        let host = format!("{}:{}", self.host, self.port);
        self.io.lock().await.log(format!("Host key fingerprint is: {} {print}", key.algorithm()));
        // a list of keys is the whole of what's trusted, as in PuTTY
        if !self.manual_keys.is_empty() && !self.is_manual(key, &print) {
            self.say(&format!("The host key for {host} isn't in this session's list of keys.\n{} {print}\n", key.algorithm()));
            return Ok(false);
        }
        let verdict = match self.is_manual(key, &print) {
            true => Verdict::Trusted,
            false => hostkeys::check(self.store.as_ref(), &self.host, self.port, key),
        };
        let yes = match verdict {
            Verdict::Trusted => true,
            Verdict::Unknown => {
                let mut io = self.io.lock().await;
                io.say(&format!(
                    "The host key for {host} is not known.\n{} {print}\n",
                    key.algorithm()
                ));
                if hostkeys::known_any(self.store.as_ref(), &self.host, self.port) {
                    io.say("\x1b[1;31mA key of a different type is on record for this host, so this may be an attack.\x1b[0m\n");
                }
                // PuTTY's three answers: Accept, Connect Once and Cancel
                let a = io.read_line("Trust this host and connect? (y to remember the key, o to connect once, n to cancel) ", true).await;
                let a = a.unwrap_or_default().trim().to_lowercase();
                let remember = matches!(a.as_str(), "y" | "yes");
                if remember {
                    self.remember(key);
                }
                remember || matches!(a.as_str(), "o" | "once")
            }
            Verdict::Changed => {
                let mut io = self.io.lock().await;
                io.say(&format!(
                    "\x1b[1;31mWARNING: THE HOST KEY FOR {host} HAS CHANGED.\x1b[0m\n\
                     Someone may be intercepting this connection, or the server's key was replaced.\n\
                     {} {print}\n",
                    key.algorithm()
                ));
                let a = io.read_line("Type yes to trust the new key, anything else to disconnect: ", true).await;
                let yes = a.is_some_and(|a| a.trim() == "yes");
                if yes {
                    self.remember(key);
                }
                yes
            }
        };
        if yes {
            self.accepted = Some(print);
        }
        self.io.lock().await.log(if yes { "Host key accepted" } else { "Host key not accepted" });
        Ok(yes)
    }

    fn remember(&self, key: &PublicKey) {
        if let Err(e) = hostkeys::remember(self.store.as_ref(), &self.host, self.port, key) {
            self.say(&format!("Could not save the host key: {e}\n"));
        }
    }
}

impl client::Handler for Client {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => self.accept(key).await,
            PublicKeyOrCertificate::Certificate(_) => {
                self.say("This host presented a certificate, which RUSTuTTY doesn't support.\n");
                Ok(false)
            }
        }
    }

    // `authenticate` holds `io` while it waits on this task, so this can't lock it.
    async fn auth_banner(&mut self, banner: &str, _: &mut client::Session) -> Result<(), Self::Error> {
        if self.banner {
            self.say(&clean(banner));
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        _address: &str,
        port: u32,
        _origin: &str,
        _origin_port: u32,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let Some((host, host_port)) = self.remote_forwards.lock().ok().and_then(|f| f.get(&(port as u16)).cloned()) else {
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        reply.accept().await;
        let events = self.events.clone();
        tokio::spawn(async move {
            match TcpStream::connect((host.as_str(), host_port)).await {
                Ok(mut tcp) => {
                    let mut ch = channel.into_stream();
                    tokio::io::copy_bidirectional(&mut ch, &mut tcp).await.ok();
                }
                Err(e) => (events)(Event::Local(format!("Forwarding to {host}:{host_port} failed: {e}\r\n").into_bytes())),
            }
        });
        Ok(())
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<client::Msg>,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        if !self.agent {
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        }
        reply.accept().await;
        tokio::spawn(async move {
            if let Ok(agent) = connect_pageant().await {
                let mut pageant = agent.dynamic().into_inner();
                let mut ch = channel.into_stream();
                tokio::io::copy_bidirectional(&mut ch, &mut pageant).await.ok();
            }
        });
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<client::Msg>,
        origin: &str,
        origin_port: u32,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let origin = origin.to_string();
        let Some(x11) = self.x11.clone() else {
            reply.reject(ChannelOpenFailure::AdministrativelyProhibited).await;
            return Ok(());
        };
        reply.accept().await;
        tokio::spawn(async move {
            let mut ch = channel.into_stream();
            if let Ok(Some(mut display)) = x11.open(&mut ch, &origin, origin_port).await {
                tokio::io::copy_bidirectional(&mut ch, &mut display).await.ok();
            }
        });
        Ok(())
    }
}

/// X11 forwarding. The server is given a made-up cookie, which is checked and
/// replaced here, so the real display's cookie never leaves this machine.
#[derive(Clone)]
struct X11 {
    /// The login the server is given: which kind, and its cookie.
    kind: &'static [u8],
    fake: [u8; 16],
    seen: Arc<std::sync::Mutex<x11auth::Seen>>,
    real: Option<(Vec<u8>, Vec<u8>)>,
    host: String,
    port: u16,
}

impl X11 {
    fn new(s: &Settings) -> anyhow::Result<X11> {
        let mut random = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .map_err(|_| anyhow::anyhow!("No random numbers available."))?;
        let xdm = s.i("X11AuthType", 1) == 2;
        let fake = if xdm { x11auth::make_cookie(random[..15].try_into()?) } else { random };
        let display = s.s("X11Display", "");
        let display = if display.is_empty() { "localhost:0".to_string() } else { display };
        let (host, number) = display.rsplit_once(':').unwrap_or((&display, "0"));
        let number: u16 = number.split('.').next().unwrap_or("0").parse().unwrap_or(0);
        let host = if host.is_empty() { "localhost" } else { host };
        let real = cookie_from_file(&s.s("X11AuthFile", ""), number);
        let kind = if xdm { x11auth::XDM } else { x11auth::MIT };
        Ok(X11 { kind, fake, seen: Default::default(), real, host: host.to_string(), port: 6000 + number })
    }

    fn fake_hex(&self) -> String {
        self.fake.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Checks the client's setup message and connects it to the display.
    async fn open<S: AsyncRead + AsyncWrite + Unpin>(&self, client: &mut S, origin: &str, origin_port: u32) -> anyhow::Result<Option<TcpStream>> {
        let mut head = [0u8; 12];
        client.read_exact(&mut head).await?;
        let big = head[0] == b'B';
        let word = |at: usize| {
            let pair = [head[at], head[at + 1]];
            (if big { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) }) as usize
        };
        let (name_len, data_len) = (word(6), word(8));
        let mut rest = vec![0u8; name_len.next_multiple_of(4) + data_len.next_multiple_of(4)];
        client.read_exact(&mut rest).await?;
        let name = &rest[..name_len];
        let data = &rest[name_len.next_multiple_of(4)..][..data_len];
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        let peer = origin.parse::<std::net::Ipv4Addr>().ok().map(|ip| (ip.octets(), origin_port as u16));
        let allowed = name == self.kind
            && if self.kind == x11auth::XDM {
                let mut seen = self.seen.lock().map_err(|_| anyhow::anyhow!("lock poisoned"))?;
                x11auth::check(&self.fake, data, peer, now, &mut seen)
            } else {
                data == self.fake
            };
        if !allowed {
            return Ok(None);
        }
        let mut display = TcpStream::connect((self.host.as_str(), self.port)).await?;
        let local = match display.local_addr()? {
            std::net::SocketAddr::V4(a) => (a.ip().octets(), a.port()),
            a => ([127, 0, 0, 1], a.port()),
        };
        let (real_name, real_data) = match &self.real {
            Some((n, d)) if n == x11auth::XDM => (n.clone(), x11auth::login_data(d, local, now)),
            Some((n, d)) => (n.clone(), d.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let mut setup = head[..6].to_vec();
        for v in [real_name.len() as u16, real_data.len() as u16] {
            setup.extend(if big { v.to_be_bytes() } else { v.to_le_bytes() });
        }
        setup.extend([0, 0]);
        for part in [&real_name, &real_data] {
            setup.extend(part);
            setup.resize(setup.len().next_multiple_of(4), 0);
        }
        display.write_all(&setup).await?;
        Ok(Some(display))
    }
}

/// The first cookie of a kind known here for this display in an Xauthority file.
fn cookie_from_file(path: &str, display: u16) -> Option<(Vec<u8>, Vec<u8>)> {
    let data = std::fs::read(path).ok()?;
    let mut p = &data[..];
    while p.len() >= 2 {
        p = &p[2..];
        let mut fields = Vec::new();
        for _ in 0..4 {
            let len = u16::from_be_bytes([*p.first()?, *p.get(1)?]) as usize;
            fields.push(p.get(2..2 + len)?);
            p = &p[2 + len..];
        }
        let number = fields[1].is_empty() || fields[1] == display.to_string().as_bytes();
        if number && (fields[2] == x11auth::MIT || fields[2] == x11auth::XDM) {
            return Some((fields[2].to_vec(), fields[3].to_vec()));
        }
    }
    None
}

fn remaining(r: &AuthResult) -> Option<Vec<MethodKind>> {
    match r {
        AuthResult::Success => None,
        AuthResult::Failure { remaining_methods, .. } => Some(remaining_methods.iter().cloned().collect()),
    }
}

async fn try_key_file(handle: &mut Handle<Client>, io: &mut Io, user: &str, path: &str, certificate: &str) -> anyhow::Result<bool> {
    let text = std::fs::read(path).unwrap_or_default();
    let key = if text.starts_with(b"PuTTY-User-Key-File") {
        match load_ppk(io, path, &String::from_utf8_lossy(&text)).await {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(false),
            Err(e) => {
                io.say(&format!("Could not load {path}: {e}\n"));
                return Ok(false);
            }
        }
    } else {
        match load_openssh(io, path).await {
            Some(k) => k,
            None => return Ok(false),
        }
    };
    if !certificate.is_empty() {
        let cert = match russh::keys::Certificate::read_file(std::path::Path::new(certificate)) {
            Ok(c) => c,
            Err(e) => {
                io.say(&format!("Could not load {certificate}: {e}\n"));
                return Ok(false);
            }
        };
        return Ok(handle.authenticate_openssh_cert(user, Arc::new(key), cert).await?.success());
    }
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
    Ok(handle.authenticate_publickey(user, key).await?.success())
}

async fn load_ppk(io: &mut Io, path: &str, text: &str) -> anyhow::Result<Option<PrivateKey>> {
    let ppk = Arc::new(Ppk::parse(text)?);
    if !ppk.encrypted {
        return ppk.to_private_key("").map(Some);
    }
    for _ in 0..MAX_TRIES {
        let Some(pass) = io.read_line(&format!("Passphrase for key \"{}\": ", clean(&ppk.comment)), false).await else {
            return Ok(None);
        };
        let pass = Zeroizing::new(pass);
        // Argon2 takes its time on purpose, which mustn't stall other sessions
        let file = ppk.clone();
        match tokio::task::spawn_blocking(move || file.to_private_key(&pass)).await? {
            Err(e) if e.to_string() == "wrong passphrase" => io.say("Wrong passphrase\n"),
            other => return other.map(Some),
        }
    }
    io.say(&format!("Giving up on {path}.\n"));
    Ok(None)
}

async fn load_openssh(io: &mut Io, path: &str) -> Option<PrivateKey> {
    match load_secret_key(path, None) {
        Ok(k) => Some(k),
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let pass = Zeroizing::new(io.read_line(&format!("Passphrase for {path}: "), false).await?);
            match load_secret_key(path, Some(pass.as_str())) {
                Ok(k) => Some(k),
                Err(e) => {
                    io.say(&format!("Could not load {path}: {e}\n"));
                    None
                }
            }
        }
        Err(e) => {
            io.say(&format!("Could not load {path}: {e}\n"));
            None
        }
    }
}

async fn try_pageant(handle: &mut Handle<Client>, io: &Io, user: &str) -> anyhow::Result<bool> {
    let Ok(mut agent) = connect_pageant().await else { return Ok(false) };
    let Ok(identities) = agent.request_identities().await else { return Ok(false) };
    let hash = handle.best_supported_rsa_hash().await?.flatten();
    for id in identities {
        let auth = handle.authenticate_publickey_with(user, id.public_key().into_owned(), hash, &mut agent).await;
        if auth.is_ok_and(|r| r.success()) {
            io.say("Authenticated with a key from Pageant.\n");
            return Ok(true);
        }
    }
    Ok(false)
}

/// Gets a plugin's answers to the server's prompts, passing on any prompts of
/// its own to the person. `None` means they gave up.
async fn plugin_answers(plugin: &mut Plugin, io: &mut Io, request: Prompts) -> anyhow::Result<Option<Vec<String>>> {
    let mut step = plugin.server_request(&request).await?;
    loop {
        match step {
            Step::Answer(answers) => return Ok(Some(answers)),
            Step::AskUser(p) => {
                for text in [&p.name, &p.instructions] {
                    if !text.is_empty() {
                        io.say(&format!("{}\n", clean(text)));
                    }
                }
                let mut answers = Vec::new();
                for (prompt, echo) in &p.prompts {
                    match io.read_line(&clean(prompt), *echo).await {
                        Some(a) => answers.push(a),
                        None => return Ok(None),
                    }
                }
                step = plugin.user_response(&answers).await?;
            }
        }
    }
}

async fn try_keyboard(handle: &mut Handle<Client>, io: &mut Io, user: &str, plugin: &mut Option<Plugin>) -> anyhow::Result<bool> {
    let mut helper = None;
    if let Some(p) = plugin.as_mut() {
        match p.offer_keyboard().await? {
            Ok(()) => helper = Some(p),
            Err(why) if why.is_empty() => {}
            Err(why) => io.say(&format!("{}\n", clean(&why))),
        }
    }
    let mut reply = handle.authenticate_keyboard_interactive_start(user, None).await?;
    loop {
        match reply {
            KeyboardInteractiveAuthResponse::Success => {
                if let Some(p) = helper {
                    p.finished(true).await?;
                }
                return Ok(true);
            }
            KeyboardInteractiveAuthResponse::Failure { partial_success, .. } => {
                if let Some(p) = helper {
                    p.finished(partial_success).await?;
                }
                return Ok(false);
            }
            KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                if let Some(p) = helper.as_deref_mut() {
                    let request = Prompts { name, instructions, prompts: prompts.into_iter().map(|q| (q.prompt, q.echo)).collect() };
                    let Some(answers) = plugin_answers(p, io, request).await? else { return Ok(false) };
                    reply = handle.authenticate_keyboard_interactive_respond(answers).await?;
                    continue;
                }
                for text in [&name, &instructions] {
                    if !text.is_empty() {
                        io.say(&format!("{}\n", clean(text)));
                    }
                }
                let mut answers = Vec::new();
                for p in prompts {
                    match io.read_line(&clean(&p.prompt), p.echo).await {
                        Some(a) => answers.push(a),
                        None => return Ok(false),
                    }
                }
                reply = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
}

async fn authenticate(handle: &mut Handle<Client>, io: &Arc<Mutex<Io>>, target: &Target) -> anyhow::Result<()> {
    let mut io = io.lock().await;
    let s = &target.settings;
    let env_user = s.b("UserNameFromEnvironment", false).then(|| std::env::var("USERNAME").ok()).flatten();
    let known_user = target.user.clone().or(env_user);
    let plugin_command = s.s("AuthPlugin", "");
    let mut plugin = None;
    let mut suggested = None;
    if !plugin_command.is_empty() {
        let (p, name) = Plugin::start(&plugin_command, &target.host, target.port, known_user.as_deref().unwrap_or("")).await?;
        plugin = Some(p);
        suggested = name;
    }
    let mut user = match suggested.or(known_user) {
        Some(u) => u,
        None => match io.read_line("login as: ", true).await {
            Some(u) if !u.is_empty() => u,
            _ => anyhow::bail!("Login cancelled."),
        },
    };
    let first = handle.authenticate_none(&user).await?;
    let Some(methods) = remaining(&first) else {
        anyhow::ensure!(!s.b("SshNoTrivialAuth", false), "The server let this session in without a login, and the session is set to refuse that.");
        return Ok(());
    };
    anyhow::ensure!(!s.b("SshNoAuth", false), "The server wants a login, and this session is set to bypass it.");
    if methods.contains(&MethodKind::PublicKey) {
        if let Some(path) = &target.key_file {
            if try_key_file(handle, &mut io, &user, path, &s.s("DetachedCertificate", "")).await? {
                return Ok(());
            }
        }
        if s.b("TryAgent", true) && try_pageant(handle, &io, &user).await? {
            return Ok(());
        }
    }
    let keyboard = s.b("AuthKI", true) && methods.contains(&MethodKind::KeyboardInteractive);
    for attempt in 0..MAX_TRIES {
        // ChangeUsername: each attempt can be under a different name
        if attempt > 0 && s.b("ChangeUsername", false) {
            match io.read_line("login as: ", true).await {
                Some(u) if !u.is_empty() => user = u,
                _ => anyhow::bail!("Login cancelled."),
            }
        }
        if keyboard {
            if try_keyboard(handle, &mut io, &user, &mut plugin).await? {
                return Ok(());
            }
        } else if methods.contains(&MethodKind::Password) {
            let prompt = format!("{user}@{}'s password: ", target.host);
            let Some(pass) = io.read_line(&prompt, false).await else {
                anyhow::bail!("Login cancelled.");
            };
            if handle.authenticate_password(&user, pass).await?.success() {
                return Ok(());
            }
        } else {
            anyhow::bail!("The server offers no login method RUSTuTTY supports.");
        }
        io.say("Access denied\n");
    }
    anyhow::bail!("Too many failed logins.")
}

fn client_config(s: &Settings, store: &dyn HostKeyStore, host: &str, port: u16) -> anyhow::Result<client::Config> {
    let bytes = algorithms::rekey_bytes(s);
    let mut config = client::Config {
        nodelay: s.b("TCPNoDelay", true),
        limits: russh::Limits::new(bytes, bytes, algorithms::rekey_time(s)),
        preferred: algorithms::preferred(s, |a| hostkeys::known(store, host, port, a))?,
        ..Default::default()
    };
    if !s.b("Compression", false) {
        config.preferred.compression = Cow::Borrowed(&[compression::NONE]);
    }
    Ok(config)
}

/// PuTTY's keepalives, every so many seconds, or none for 0. They're sent
/// here rather than by russh so a change of settings can reach them.
pub(crate) fn keepalives(seconds: u32) -> Option<tokio::time::Interval> {
    (seconds > 0).then(|| {
        let every = Duration::from_secs(seconds as u64);
        let mut t = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        t
    })
}

/// The next keepalive, or never.
pub(crate) async fn tick(ping: &mut Option<tokio::time::Interval>) {
    match ping {
        Some(t) => {
            t.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// `LogHost`, which can be `host`, `host:port`, `[v6]:port` or a bare IPv6 address.
fn log_host(value: &str, port: u16) -> (String, u16) {
    if let Some(rest) = value.strip_prefix('[') {
        if let Some((host, tail)) = rest.split_once(']') {
            return (host.to_string(), tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(port));
        }
    }
    match value.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().unwrap_or(port)),
        _ => (value.to_string(), port),
    }
}

/// Connects and logs in, for sessions and for SSH proxies alike. Only a `full`
/// session asks the server for forwarding, so only it will accept the channels:
/// file transfers and proxies never offer the server a way to Pageant.
/// Where each remote forwarding's connections go, by the remote port. Shared,
/// so a session joining a connection can add its own.
type RemoteForwards = Arc<StdMutex<HashMap<u16, (String, u16)>>>;

async fn login(target: &Target, store: Arc<dyn HostKeyStore>, io: Arc<Mutex<Io>>, x11: Option<X11>, full: bool) -> anyhow::Result<Handle<Client>> {
    Ok(login_with(target, store, io, x11, full).await?.0)
}

/// Logs in, and gives back the connection's remote forwardings too.
async fn login_with(target: &Target, store: Arc<dyn HostKeyStore>, io: Arc<Mutex<Io>>, x11: Option<X11>, full: bool) -> anyhow::Result<(Handle<Client>, RemoteForwards)> {
    let s = &target.settings;
    anyhow::ensure!(s.i("SshProt", 3) != 0, "This session is set to SSH-1 only, which RUSTuTTY doesn't support.");
    let events = io.lock().await.events.clone();
    let logical = s.s("LogHost", "");
    let (key_host, key_port) = if logical.is_empty() { (target.host.clone(), target.port) } else { log_host(&logical, target.port) };
    let config = Arc::new(client_config(s, store.as_ref(), &key_host, key_port)?);
    let remote_forwards = s
        .forwards()
        .into_iter()
        .filter_map(|f| match f {
            Forward::Remote { port, host, host_port, .. } if full => Some((port, (host, host_port))),
            _ => None,
        })
        .collect();
    let remote_forwards: RemoteForwards = Arc::new(StdMutex::new(remote_forwards));
    let forwards = remote_forwards.clone();
    let client = Client {
        host: key_host,
        port: key_port,
        store: store.clone(),
        io: io.clone(),
        events,
        accepted: None,
        manual_keys: s.map("SSHManualHostKeys").into_iter().map(|(k, _)| k).collect(),
        banner: s.b("SshBanner", true),
        remote_forwards,
        agent: full && s.b("AgentFwd", false) && !s.b("SshNoShell", false),
        x11,
    };
    let stream = proxy::connect(target, &store, &io).await?;
    let mut handle = client::connect_stream(config, stream, client).await.map_err(|e| {
        anyhow::anyhow!("SSH connection failed: {e}")
    })?;
    io.lock().await.log("Using SSH protocol version 2");
    let result = authenticate(&mut handle, &io, target).await;
    if result.is_ok() {
        io.lock().await.log("Access granted");
    }
    if let Err(e) = result {
        // russh reports only "send error" once the connection has gone, and
        // keeps the real reason in the session task
        if handle.is_closed() {
            if let Err(real) = (&mut handle).await {
                anyhow::bail!("Connection lost during login: {real:#}");
            }
            anyhow::bail!("The server closed the connection during login.");
        }
        return Err(e);
    }
    Ok((handle, forwards))
}

pub(crate) enum Tunnel {
    Tcp(String, u16),
    Exec(String),
    Subsystem(String),
}

/// A connection through an SSH proxy, which has to stay up as long as this does.
struct Tunneled {
    _handle: Handle<Client>,
    stream: ChannelStream<client::Msg>,
}

impl AsyncRead for Tunneled {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tunneled {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

// Boxed because a proxy can itself be reached through a proxy.
pub(crate) fn tunnel(jump: Target, how: Tunnel, store: Arc<dyn HostKeyStore>, io: Arc<Mutex<Io>>) -> Pin<Box<dyn Future<Output = anyhow::Result<Conn>> + Send>> {
    Box::pin(async move {
        let handle = login(&jump, store, io, None, false).await?;
        let channel = match how {
            Tunnel::Tcp(host, port) => handle.channel_open_direct_tcpip(host, port as u32, "127.0.0.1", 0).await?,
            Tunnel::Exec(command) => {
                let c = handle.channel_open_session().await?;
                c.exec(true, command).await?;
                c
            }
            Tunnel::Subsystem(name) => {
                let c = handle.channel_open_session().await?;
                c.request_subsystem(true, name).await?;
                c
            }
        };
        Ok(Box::new(Tunneled { _handle: handle, stream: channel.into_stream() }) as Conn)
    })
}

/// A remote command's input and output, for the old SCP protocol.
pub struct Exec {
    _handle: Handle<Client>,
    pub stream: ChannelStream<client::Msg>,
}

pub(crate) async fn exec(target: &Target, store: Arc<dyn HostKeyStore>, io: Io, command: &str) -> anyhow::Result<Exec> {
    let handle = login(target, store, Arc::new(Mutex::new(io)), None, false).await?;
    let channel = handle.channel_open_session().await?;
    channel.exec(true, command).await?;
    Ok(Exec { _handle: handle, stream: channel.into_stream() })
}

/// An SFTP session and the connection it runs over.
pub struct Sftp {
    _handle: Handle<Client>,
    pub session: SftpSession,
}

pub(crate) async fn sftp(target: &Target, store: Arc<dyn HostKeyStore>, io: Io) -> anyhow::Result<Sftp> {
    let handle = login(target, store, Arc::new(Mutex::new(io)), None, false).await?;
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    let session = SftpSession::new(channel.into_stream()).await.map_err(|e| anyhow::anyhow!("The server has no SFTP: {e}"))?;
    Ok(Sftp { _handle: handle, session })
}

fn listen_address(bind: Option<String>, accept_all: bool) -> String {
    bind.unwrap_or_else(|| if accept_all { "0.0.0.0" } else { "127.0.0.1" }.to_string())
}

/// The longest SOCKS 4 user name or host name read: a host name has at most
/// 255 bytes, as in SOCKS 5, and anything longer is refused, not stored.
const SOCKS_FIELD: usize = 255;
/// How long a SOCKS client gets to say where it's going.
const SOCKS_WAIT: Duration = Duration::from_secs(10);

async fn zero_terminated(c: &mut TcpStream) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        match c.read_u8().await? {
            0 => return Ok(out),
            _ if out.len() == SOCKS_FIELD => anyhow::bail!("SOCKS field too long"),
            b => out.push(b),
        }
    }
}

/// What a SOCKS client wants to reach, and whether it spoke version 5. A
/// client that takes too long is dropped, so a stalled one holds nothing.
async fn socks_destination(c: &mut TcpStream, wait: Duration) -> anyhow::Result<(String, u16, bool)> {
    tokio::time::timeout(wait, socks_request(c)).await.map_err(|_| anyhow::anyhow!("SOCKS client too slow"))?
}

async fn socks_request(c: &mut TcpStream) -> anyhow::Result<(String, u16, bool)> {
    match c.read_u8().await? {
        4 => {
            anyhow::ensure!(c.read_u8().await? == 1, "only SOCKS connect is supported");
            let port = c.read_u16().await?;
            let mut ip = [0u8; 4];
            c.read_exact(&mut ip).await?;
            zero_terminated(c).await?;
            let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
                String::from_utf8_lossy(&zero_terminated(c).await?).into_owned()
            } else {
                std::net::Ipv4Addr::from(ip).to_string()
            };
            Ok((host, port, false))
        }
        5 => {
            let mut methods = vec![0u8; c.read_u8().await? as usize];
            c.read_exact(&mut methods).await?;
            c.write_all(&[5, 0]).await?;
            let mut head = [0u8; 4];
            c.read_exact(&mut head).await?;
            anyhow::ensure!(head[1] == 1, "only SOCKS connect is supported");
            let host = match head[3] {
                1 => {
                    let mut ip = [0u8; 4];
                    c.read_exact(&mut ip).await?;
                    std::net::Ipv4Addr::from(ip).to_string()
                }
                4 => {
                    let mut ip = [0u8; 16];
                    c.read_exact(&mut ip).await?;
                    std::net::Ipv6Addr::from(ip).to_string()
                }
                3 => {
                    let mut name = vec![0u8; c.read_u8().await? as usize];
                    c.read_exact(&mut name).await?;
                    String::from_utf8_lossy(&name).into_owned()
                }
                n => anyhow::bail!("unknown SOCKS address type {n}"),
            };
            Ok((host, c.read_u16().await?, true))
        }
        n => anyhow::bail!("unknown SOCKS version {n}"),
    }
}

async fn serve_local(handle: Arc<Handle<Client>>, listener: TcpListener, dest: Option<(String, u16)>) {
    while let Ok((mut tcp, peer)) = listener.accept().await {
        let (handle, dest) = (handle.clone(), dest.clone());
        tokio::spawn(async move {
            let (host, port, socks) = match dest {
                Some((h, p)) => (h, p, None),
                None => match socks_destination(&mut tcp, SOCKS_WAIT).await {
                    Ok((h, p, five)) => (h, p, Some(five)),
                    Err(_) => return,
                },
            };
            let opened = handle.channel_open_direct_tcpip(host, port as u32, peer.ip().to_string(), peer.port() as u32).await;
            let reply: &[u8] = match (socks, opened.is_ok()) {
                (Some(true), true) => &[5, 0, 0, 1, 0, 0, 0, 0, 0, 0],
                (Some(true), false) => &[5, 1, 0, 1, 0, 0, 0, 0, 0, 0],
                (Some(false), true) => &[0, 90, 0, 0, 0, 0, 0, 0],
                (Some(false), false) => &[0, 91, 0, 0, 0, 0, 0, 0],
                (None, _) => &[],
            };
            if tcp.write_all(reply).await.is_err() {
                return;
            }
            if let Ok(channel) = opened {
                let mut ch = channel.into_stream();
                tokio::io::copy_bidirectional(&mut ch, &mut tcp).await.ok();
            }
        });
    }
}

/// Sets up the session's forwardings. A failure is reported, not fatal, as in PuTTY.
async fn start_forwards(handle: &Arc<Handle<Client>>, remote: &RemoteForwards, s: &Settings, say: impl Fn(&str)) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::new();
    let local_all = s.b("LocalPortAcceptAll", false);
    for f in s.forwards() {
        let (bind, port, dest) = match f {
            Forward::Remote { bind, port, host, host_port } => {
                // where its connections go, for this connection's handler
                if let Ok(mut all) = remote.lock() {
                    all.insert(port, (host, host_port));
                }
                let address = listen_address(bind, s.b("RemotePortAcceptAll", false));
                if let Err(e) = handle.tcpip_forward(address, port as u32).await {
                    say(&format!("Remote port {port} could not be forwarded: {e}\n"));
                }
                continue;
            }
            Forward::Local { bind, port, host, host_port } => (bind, port, Some((host, host_port))),
            Forward::Dynamic { bind, port } => (bind, port, None),
        };
        match TcpListener::bind((listen_address(bind, local_all).as_str(), port)).await {
            Ok(l) => tasks.push(tokio::spawn(serve_local(handle.clone(), l, dest))),
            Err(e) => say(&format!("Local port {port} could not be forwarded: {e}\n")),
        }
    }
    tasks
}

/// Live connections open to share, by user, host and port. A session started
/// with `ConnectionSharing` joins one here instead of logging in again. This
/// works between sessions in one program: it doesn't speak PuTTY's own sharing
/// protocol, so it can't join a connection held by a separate PuTTY.
static SHARED: LazyLock<StdMutex<HashMap<String, (Weak<Handle<Client>>, RemoteForwards)>>> = LazyLock::new(Default::default);

/// Two sessions share only if they'd have made the same connection, with the
/// same checks. A login that can end up under another name is never shared.
fn share_key(target: &Target) -> Option<String> {
    let user = target.user.as_ref()?;
    let s = &target.settings;
    if !s.b("ConnectionSharing", false) || s.b("ChangeUsername", false) || !s.s("AuthPlugin", "").is_empty() {
        return None;
    }
    let text = ["ProxyHost", "ProxyUsername", "ProxyTelnetCommand", "LogHost"].map(|k| s.s(k, ""));
    let numbers = ["ProxyMethod", "ProxyPort", "AgentFwd", "X11Forward"].map(|k| s.i(k, 0));
    let manual = s.map("SSHManualHostKeys");
    Some(format!("{user}@{}:{} {text:?} {numbers:?} {:?} {manual:?}", target.host, target.port, target.key_file))
}

fn shared(key: &str) -> Option<(Arc<Handle<Client>>, RemoteForwards)> {
    let map = SHARED.lock().ok()?;
    let (handle, forwards) = map.get(key)?;
    Some((handle.upgrade().filter(|h| !h.is_closed())?, forwards.clone()))
}

pub(crate) async fn run(target: &Target, store: Arc<dyn HostKeyStore>, io: Io) -> anyhow::Result<String> {
    let s = &target.settings;
    let x11 = s.b("X11Forward", false).then(|| X11::new(s)).transpose()?;
    let io = Arc::new(Mutex::new(io));
    let key = share_key(target);
    let joined = key.as_deref().filter(|_| s.b("ConnectionSharingDownstream", true)).and_then(shared);
    let (handle, remote) = match joined {
        Some(joined) => {
            io.lock().await.say("Sharing an existing connection.\n");
            joined
        }
        None => {
            let (h, remote) = login_with(target, store, io.clone(), x11.clone(), true).await?;
            let h = Arc::new(h);
            if let (Some(k), true, Ok(mut map)) = (key, s.b("ConnectionSharingUpstream", true), SHARED.lock()) {
                map.retain(|_, (w, _)| w.strong_count() > 0);
                map.insert(k, (Arc::downgrade(&h), remote.clone()));
            }
            (h, remote)
        }
    };
    let mut io = io.lock().await;
    // a session joining a shared connection sets up its own forwardings on it
    for f in s.forwards() {
        io.log(match f {
            Forward::Local { port, host, host_port, .. } => format!("Local port {port} forwarding to {host}:{host_port}"),
            Forward::Remote { port, host, host_port, .. } => format!("Requesting remote port {port} forward to {host}:{host_port}"),
            Forward::Dynamic { port, .. } => format!("Local port {port} SOCKS dynamic forwarding"),
        });
    }
    let forwards = start_forwards(&handle, &remote, s, |t| io.say(t)).await;

    // PuTTY's "Don't start a shell or command at all", for forwarding only
    if s.b("SshNoShell", false) {
        io.say("Connected, with no shell. Close the session to disconnect.\n");
        let mut ping = keepalives(s.ping_seconds());
        loop {
            tokio::select! {
                _ = tick(&mut ping) => handle.send_keepalive(false).await?,
                cmd = io.commands.recv() => match cmd {
                    None | Some(Command::Close) => break,
                    Some(Command::Reconfigure(new)) => ping = keepalives(new.settings.ping_seconds()),
                    Some(_) => {}
                },
            }
        }
        forwards.iter().for_each(JoinHandle::abort);
        handle.disconnect(russh::Disconnect::ByApplication, "", "en").await.ok();
        return Ok("Session closed.".into());
    }

    let mut channel = handle.channel_open_session().await?;
    io.log("Opened main channel");
    // the window may have been resized while the login prompts were up
    let (mut cols, mut rows) = (io.cols, io.rows);
    if !s.b("NoPTY", false) {
        let modes = algorithms::terminal_modes(s);
        channel.request_pty(false, &s.s("TerminalType", "xterm"), cols, rows, 0, 0, &modes).await?;
        io.log("Allocated pty");
    }
    for (k, v) in s.map("Environment") {
        channel.set_env(false, k, v).await?;
    }
    if s.b("AgentFwd", false) {
        channel.agent_forward(false).await?;
    }
    if let Some(x11) = &x11 {
        let screen = s.s("X11Display", "").rsplit_once('.').and_then(|(_, n)| n.parse().ok()).unwrap_or(0);
        let name = String::from_utf8_lossy(x11.kind).into_owned();
        channel.request_x11(false, false, name, x11.fake_hex(), screen).await?;
    }
    let command = s.s("RemoteCommand", "");
    if command.is_empty() {
        channel.request_shell(false).await?;
        io.log("Started a shell");
    } else {
        channel.exec(false, command).await?;
        io.log("Started a command");
    }

    io.logged_in();
    let mut status = None;
    let mut ping = keepalives(s.ping_seconds());
    let reason = loop {
        tokio::select! {
            _ = tick(&mut ping) => handle.send_keepalive(false).await?,
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { data }) => (io.events)(Event::Output(data.to_vec())),
                Some(ChannelMsg::ExtendedData { data, .. }) => (io.events)(Event::Output(data.to_vec())),
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    io.log(format!("Session sent command exit status {exit_status}"));
                    (io.events)(Event::ExitStatus(exit_status));
                    status = Some(exit_status);
                }
                Some(ChannelMsg::Close) | None => break match status {
                    Some(0) | None => "Server closed the session.".to_string(),
                    Some(n) => format!("Session ended with exit status {n}."),
                },
                Some(_) => {}
            },
            cmd = io.commands.recv() => match cmd {
                None | Some(Command::Close) => {
                    channel.close().await.ok();
                    break "Session closed.".to_string();
                }
                Some(Command::Input(bytes) | Command::Reply(bytes)) => channel.data_bytes(bytes).await?,
                Some(Command::Special(crate::session::Special::Signal(name))) => channel.signal(russh::Sig::Custom(name.to_string())).await?,
                Some(Command::Special(crate::session::Special::Rekey)) => handle.rekey_soon().await?,
                // russh can't send IGNORE: a keepalive also keeps the connection busy
                Some(Command::Special(crate::session::Special::Ignore)) => handle.send_keepalive(false).await?,
                // Change Settings: the keepalive interval takes effect at once, as PuTTY's
                Some(Command::Reconfigure(new)) => ping = keepalives(new.settings.ping_seconds()),
                Some(Command::Special(_)) => {}
                Some(Command::Resize { cols: c, rows: r }) => {
                    if (c, r) != (cols, rows) {
                        (cols, rows) = (c, r);
                        channel.window_change(c, r, 0, 0).await?;
                    }
                }
            },
        }
    };
    forwards.iter().for_each(JoinHandle::abort);
    // the connection goes when the last session sharing it does
    if Arc::strong_count(&handle) == 1 {
        handle.disconnect(russh::Disconnect::ByApplication, "", "en").await.ok();
    }
    Ok(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SOCKS client on one end of a local connection, and the parser on the other.
    async fn ask(request: Vec<u8>) -> anyhow::Result<(String, u16, bool)> {
        ask_within(request, SOCKS_WAIT).await
    }

    async fn ask_within(request: Vec<u8>, wait: Duration) -> anyhow::Result<(String, u16, bool)> {
        ask_in_pieces(vec![request], wait).await
    }

    /// Sends each piece separately, with a pause between, as a slow client might.
    async fn ask_in_pieces(pieces: Vec<Vec<u8>>, wait: Duration) -> anyhow::Result<(String, u16, bool)> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.unwrap();
            c.set_nodelay(true).unwrap();
            for piece in pieces {
                let _ = c.write_all(&piece).await;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // keep the connection open until the parser is done with it
            let _ = c.read_u8().await;
        });
        let (mut server, _) = listener.accept().await.unwrap();
        let got = socks_destination(&mut server, wait).await;
        drop(server);
        client.await.unwrap();
        got
    }

    #[tokio::test]
    async fn socks4a_names_are_read() {
        let mut req = vec![4, 1, 0, 80, 0, 0, 0, 1];
        req.extend(b"user\0example.test\0");
        assert_eq!(ask(req).await.unwrap(), ("example.test".to_string(), 80, false));
    }

    #[tokio::test]
    async fn socks4_fields_are_bounded() {
        let mut req = vec![4, 1, 0, 80, 0, 0, 0, 1];
        req.extend(vec![b'u'; 64 * 1024]);
        let e = ask(req).await.unwrap_err();
        assert!(e.to_string().contains("too long"), "{e}");
    }

    #[tokio::test]
    async fn socks4_only_connects() {
        let e = ask(vec![4, 2, 0, 80, 127, 0, 0, 1, 0]).await.unwrap_err();
        assert!(e.to_string().contains("connect"), "{e}");
    }

    /// PuTTY d2590d98: an address arriving in parts must still be read whole.
    #[tokio::test]
    async fn a_socks5_request_in_pieces_is_read_whole() {
        let mut ip6 = vec![0x20, 0x01, 0x0d, 0xb8];
        ip6.extend([0; 11]);
        ip6.push(1);
        let pieces = vec![vec![5, 1, 0], vec![5, 1, 0, 4], ip6[..7].to_vec(), ip6[7..].to_vec(), vec![0], vec![22]];
        assert_eq!(ask_in_pieces(pieces, SOCKS_WAIT).await.unwrap(), ("2001:db8::1".to_string(), 22, true));
        let mut name = vec![5, 1, 0, 3, 12];
        name.extend(b"examp");
        let pieces = vec![vec![5, 1, 0], name, b"le.test".to_vec(), vec![0, 80]];
        assert_eq!(ask_in_pieces(pieces, SOCKS_WAIT).await.unwrap(), ("example.test".to_string(), 80, true));
    }

    #[tokio::test]
    async fn a_stalled_socks_client_is_dropped() {
        let e = ask_within(vec![4, 1, 0, 80], Duration::from_millis(200)).await.unwrap_err();
        assert!(e.to_string().contains("too slow"), "{e}");
    }
}

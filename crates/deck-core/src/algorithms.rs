//! PuTTY's algorithm preference lists and terminal modes, in russh's terms.
//! Anything PuTTY lists below `WARN` would make it ask first. There's no place
//! to ask in russh's negotiation, so those aren't offered at all, which fails
//! closed. PuTTY's names with no russh equivalent are skipped.

use crate::settings::Settings;
use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
use russh::{cipher, kex, Preferred, Pty};
use std::borrow::Cow;
use std::time::Duration;

/// What an unset list means here. The settings window shows these too.
pub const DEFAULT_CIPHERS: &str = "aes,chacha20,aesgcm,3des,WARN";
pub const DEFAULT_KEXES: &str = "mlkem-curve25519,ecdh,dh-gex-sha1,dh-group18-sha512,dh-group17-sha512,dh-group16-sha512,dh-group15-sha512,dh-group14-sha1,WARN";
pub const DEFAULT_HOST_KEYS: &str = "ed25519,ecdsa,rsa,WARN";

/// One entry of a preference list: its saved name, label, and where it goes
/// if a session doesn't mention it (`keyvalwhere` in settings.c).
pub struct Pref(pub &'static str, pub &'static str, pub Option<&'static str>, pub i32);

pub const CIPHERS: &[Pref] = &[
    Pref("aes", "AES (SSH-2 only)", None, -1),
    Pref("chacha20", "ChaCha20 (SSH-2 only)", Some("aes"), 1),
    Pref("aesgcm", "AES-GCM (SSH-2 only)", Some("chacha20"), 1),
    Pref("3des", "3DES", None, -1),
    Pref("WARN", "-- warn below here --", None, -1),
    Pref("des", "DES", None, -1),
    Pref("blowfish", "Blowfish", None, -1),
    Pref("arcfour", "Arcfour (SSH-2 only)", None, -1),
];

pub const KEXES: &[Pref] = &[
    Pref("ntru-curve25519", "NTRU Prime / Curve25519 hybrid kex", None, 1),
    Pref("mlkem-curve25519", "ML-KEM / Curve25519 hybrid kex", Some("ntru-curve25519"), 1),
    Pref("mlkem-nist", "ML-KEM / NIST ECDH hybrid kex", Some("mlkem-curve25519"), 1),
    Pref("ecdh", "ECDH key exchange", None, 1),
    Pref("dh-gex-sha1", "Diffie-Hellman group exchange", None, -1),
    Pref("dh-group14-sha1", "Diffie-Hellman group 14 (2048-bit)", None, -1),
    Pref("dh-group1-sha1", "Diffie-Hellman group 1 (1024-bit)", Some("WARN"), 1),
    Pref("rsa", "RSA-based key exchange", Some("WARN"), -1),
    Pref("dh-group15-sha512", "Diffie-Hellman group 15 (3072-bit)", Some("dh-group14-sha1"), -1),
    Pref("dh-group16-sha512", "Diffie-Hellman group 16 (4096-bit)", Some("dh-group15-sha512"), -1),
    Pref("dh-group17-sha512", "Diffie-Hellman group 17 (6144-bit)", Some("dh-group16-sha512"), 1),
    Pref("dh-group18-sha512", "Diffie-Hellman group 18 (8192-bit)", Some("dh-group17-sha512"), 1),
    Pref("WARN", "-- warn below here --", None, -1),
];

pub const HOST_KEYS: &[Pref] = &[
    Pref("ed25519", "Ed25519", None, 1),
    Pref("ed448", "Ed448", None, 1),
    Pref("ecdsa", "ECDSA", None, -1),
    Pref("dsa", "DSA", None, -1),
    Pref("rsa", "RSA", None, -1),
    Pref("WARN", "-- warn below here --", None, -1),
];

/// `gprefs_from_str`: the order from a saved list, with anything it leaves out
/// put where the table says.
pub fn prefs_order(text: &str, table: &'static [Pref]) -> Vec<usize> {
    let at = |name: &str| table.iter().position(|p| p.0 == name);
    let mut out: Vec<usize> = Vec::new();
    for word in text.split(',') {
        if let Some(i) = at(word) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    while out.len() < table.len() {
        for (i, p) in table.iter().enumerate() {
            if out.contains(&i) {
                continue;
            }
            let pos = match p.2 {
                None if p.3 < 0 => out.len(),
                None => 0,
                Some(rel) => match out.iter().position(|&j| Some(j) == at(rel)) {
                    Some(j) if p.3 < 0 => j,
                    Some(j) => j + 1,
                    None => continue,
                },
            };
            out.insert(pos, i);
        }
    }
    out
}

/// A saved list as PuTTY reads it: anything a session saved before PuTTY
/// knew of it goes where the table says, so an older session still gets newer
/// algorithms, and the settings window and the connection agree.
fn filled(s: &Settings, key: &str, default: &str, table: &'static [Pref]) -> String {
    let order = prefs_order(&s.s(key, default), table);
    order.iter().map(|&i| table[i].0).collect::<Vec<_>>().join(",")
}

/// russh needs these in the key exchange list, as they turn on extensions.
const KEX_EXTENSIONS: [kex::Name; 1] = [kex::EXTENSION_SUPPORT_AS_CLIENT];

fn above_warn<'a>(s: &'a str) -> impl Iterator<Item = &'a str> {
    s.split(',').map(str::trim).take_while(|n| *n != "WARN")
}

fn ciphers(list: &str) -> Vec<cipher::Name> {
    above_warn(list)
        .flat_map(|name| -> &[cipher::Name] {
            match name {
                "aes" => &[cipher::AES_256_CTR, cipher::AES_192_CTR, cipher::AES_128_CTR, cipher::AES_256_CBC, cipher::AES_192_CBC, cipher::AES_128_CBC],
                "chacha20" => &[cipher::CHACHA20_POLY1305],
                "aesgcm" => &[cipher::AES_256_GCM, cipher::AES_128_GCM],
                "3des" => &[cipher::TRIPLE_DES_CBC],
                _ => &[],
            }
        })
        .copied()
        .collect()
}

fn kexes(list: &str) -> Vec<kex::Name> {
    above_warn(list)
        .flat_map(|name| -> &[kex::Name] {
            match name {
                "mlkem-curve25519" => &[kex::MLKEM768X25519_SHA256],
                "ecdh" => &[kex::CURVE25519, kex::CURVE25519_PRE_RFC_8731, kex::ECDH_SHA2_NISTP256, kex::ECDH_SHA2_NISTP384, kex::ECDH_SHA2_NISTP521],
                "dh-gex-sha1" => &[kex::DH_GEX_SHA256, kex::DH_GEX_SHA1],
                "dh-group18-sha512" => &[kex::DH_G18_SHA512],
                "dh-group17-sha512" => &[kex::DH_G17_SHA512],
                "dh-group16-sha512" => &[kex::DH_G16_SHA512],
                "dh-group15-sha512" => &[kex::DH_G15_SHA512],
                "dh-group14-sha1" => &[kex::DH_G14_SHA256, kex::DH_G14_SHA1],
                "dh-group1-sha1" => &[kex::DH_G1_SHA1],
                _ => &[],
            }
        })
        .copied()
        .collect()
}

fn host_keys(list: &str) -> Vec<Algorithm> {
    let ecdsa = |curve| Algorithm::Ecdsa { curve };
    above_warn(list)
        .flat_map(|name| match name {
            "ed25519" => vec![Algorithm::Ed25519],
            "ecdsa" => vec![ecdsa(EcdsaCurve::NistP256), ecdsa(EcdsaCurve::NistP384), ecdsa(EcdsaCurve::NistP521)],
            "rsa" => vec![
                Algorithm::Rsa { hash: Some(HashAlg::Sha512) },
                Algorithm::Rsa { hash: Some(HashAlg::Sha256) },
                Algorithm::Rsa { hash: None },
            ],
            _ => Vec::new(),
        })
        .collect()
}

/// The negotiation lists for a session. `known` says whether a host key of an
/// algorithm is already trusted: those go first (`PreferKnownHostKeys`), so a
/// host isn't reported as unknown for offering a newer key type first.
///
/// A list with nothing usable above `WARN` is refused rather than replaced
/// with russh's defaults, which would use algorithms the settings left out.
pub fn preferred(s: &Settings, known: impl Fn(&Algorithm) -> bool) -> anyhow::Result<Preferred> {
    let none = |what: &str, page: &str| {
        anyhow::anyhow!("This session allows no {what} RUSTuTTY can use: move one above \"warn below here\" in Connection > SSH > {page}.")
    };
    let cipher = ciphers(&filled(s, "Cipher", DEFAULT_CIPHERS, CIPHERS));
    anyhow::ensure!(!cipher.is_empty(), none("cipher", "Cipher"));
    let mut kex = kexes(&filled(s, "KEX", DEFAULT_KEXES, KEXES));
    anyhow::ensure!(!kex.is_empty(), none("key exchange", "Kex"));
    kex.extend(KEX_EXTENSIONS);
    kex.push(kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT);
    let mut keys = host_keys(&filled(s, "HostKey", DEFAULT_HOST_KEYS, HOST_KEYS));
    anyhow::ensure!(!keys.is_empty(), none("host key type", "Host keys"));
    if s.b("PreferKnownHostKeys", true) {
        keys.sort_by_key(|k| !known(k));
    }
    Ok(Preferred { cipher: Cow::Owned(cipher), kex: Cow::Owned(kex), key: Cow::Owned(keys), ..Preferred::DEFAULT })
}

/// `RekeyBytes`: a number with an optional K, M or G. `0` means never, but
/// russh's safety limit of 1G still applies.
pub fn rekey_bytes(s: &Settings) -> usize {
    const MAX: u64 = 1 << 30;
    let text = s.s("RekeyBytes", "1G");
    let text = text.trim();
    let (digits, scale) = match text.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&text[..text.len() - 1], 1 << 10),
        Some('M') => (&text[..text.len() - 1], 1 << 20),
        Some('G') => (&text[..text.len() - 1], 1 << 30),
        _ => (text, 1),
    };
    match digits.parse::<u64>().map(|n| n.saturating_mul(scale)) {
        Ok(0) | Err(_) => MAX as usize,
        Ok(n) => n.min(MAX) as usize,
    }
}

pub fn rekey_time(s: &Settings) -> Duration {
    match s.i("RekeyTime", 60) {
        0 => Duration::from_secs(u32::MAX as u64),
        minutes => Duration::from_secs(minutes as u64 * 60),
    }
}

fn special_char(v: &str) -> Option<u32> {
    match v.as_bytes() {
        [] => None,
        [b'^', b'?'] => Some(127),
        [b'^', b'<', ..] => v[2..].trim_end_matches('>').parse().ok(),
        [b'^', c] => Some((c.to_ascii_uppercase() & 0x1f) as u32),
        [c] => Some(*c as u32),
        _ => v.parse().ok(),
    }
}

fn flag(v: &str) -> Option<u32> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "on" | "true" | "1" | "+" => Some(1),
        "no" | "off" | "false" | "0" | "-" => Some(0),
        _ => None,
    }
}

const CHAR_MODES: [(&str, Pty); 15] = [
    ("INTR", Pty::VINTR), ("QUIT", Pty::VQUIT), ("ERASE", Pty::VERASE), ("KILL", Pty::VKILL), ("EOF", Pty::VEOF),
    ("EOL", Pty::VEOL), ("EOL2", Pty::VEOL2), ("START", Pty::VSTART), ("STOP", Pty::VSTOP), ("SUSP", Pty::VSUSP),
    ("DSUSP", Pty::VDSUSP), ("REPRINT", Pty::VREPRINT), ("WERASE", Pty::VWERASE), ("LNEXT", Pty::VLNEXT), ("DISCARD", Pty::VDISCARD),
];
const FLAG_MODES: [(&str, Pty); 37] = [
    ("IGNPAR", Pty::IGNPAR), ("PARMRK", Pty::PARMRK), ("INPCK", Pty::INPCK), ("ISTRIP", Pty::ISTRIP), ("INLCR", Pty::INLCR),
    ("IGNCR", Pty::IGNCR), ("ICRNL", Pty::ICRNL), ("IUCLC", Pty::IUCLC), ("IXON", Pty::IXON), ("IXANY", Pty::IXANY),
    ("IXOFF", Pty::IXOFF), ("IMAXBEL", Pty::IMAXBEL), ("IUTF8", Pty::IUTF8), ("ISIG", Pty::ISIG), ("ICANON", Pty::ICANON),
    ("XCASE", Pty::XCASE), ("ECHO", Pty::ECHO), ("ECHOE", Pty::ECHOE), ("ECHOK", Pty::ECHOK), ("ECHONL", Pty::ECHONL),
    ("NOFLSH", Pty::NOFLSH), ("TOSTOP", Pty::TOSTOP), ("IEXTEN", Pty::IEXTEN), ("ECHOCTL", Pty::ECHOCTL), ("ECHOKE", Pty::ECHOKE),
    ("PENDIN", Pty::PENDIN), ("OPOST", Pty::OPOST), ("OLCUC", Pty::OLCUC), ("ONLCR", Pty::ONLCR), ("OCRNL", Pty::OCRNL),
    ("ONOCR", Pty::ONOCR), ("ONLRET", Pty::ONLRET), ("CS7", Pty::CS7), ("CS8", Pty::CS8), ("PARENB", Pty::PARENB),
    ("PARODD", Pty::PARODD), ("SWTCH", Pty::VSWTCH),
];

/// `TerminalModes`: each value is `A` for automatic, `N` to not send it, or
/// `V` and a value. Automatic only means something for these two, as in PuTTY.
pub fn terminal_modes(s: &Settings) -> Vec<(Pty, u32)> {
    let mut out = Vec::new();
    let saved = s.map("TerminalModes");
    let get = |name: &str| saved.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    let erase = if s.b("BackspaceIsDelete", true) { 127 } else { 8 };
    match get("ERASE") {
        None | Some("A") => out.push((Pty::VERASE, erase)),
        Some(_) => {}
    }
    match get("IUTF8") {
        None | Some("A") if s.s("LineCodePage", "").is_empty() || s.s("LineCodePage", "").contains("UTF-8") => out.push((Pty::IUTF8, 1)),
        _ => {}
    }
    for (name, value) in &saved {
        let Some(v) = value.strip_prefix('V') else { continue };
        if let Some((_, pty)) = CHAR_MODES.iter().find(|(n, _)| n == name) {
            if let Some(c) = special_char(v) {
                out.push((*pty, c));
            }
        } else if let Some((_, pty)) = FLAG_MODES.iter().find(|(n, _)| n == name) {
            if let Some(f) = flag(v) {
                out.push((*pty, f));
            }
        }
    }
    let speed = s.s("TerminalSpeed", "38400,38400");
    let mut speeds = speed.split(',').map(|n| n.trim().parse::<u32>());
    if let (Some(Ok(o)), Some(Ok(i))) = (speeds.next(), speeds.next()) {
        out.push((Pty::TTY_OP_OSPEED, o));
        out.push((Pty::TTY_OP_ISPEED, i));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    fn with(key: &str, v: &str) -> Settings {
        let mut s = Settings::default();
        s.set(key, Value::Str(v.into()));
        s
    }

    #[test]
    fn order_is_kept_and_nothing_below_warn_is_offered() {
        let p = preferred(&with("Cipher", "chacha20,aes,WARN,3des,arcfour"), |_| false).unwrap();
        // AES-GCM isn't named, so it goes after ChaCha20, as in PuTTY
        assert_eq!(p.cipher[0], cipher::CHACHA20_POLY1305);
        assert_eq!(p.cipher[1], cipher::AES_256_GCM);
        assert!(p.cipher.contains(&cipher::AES_256_CTR));
        assert!(!p.cipher.contains(&cipher::TRIPLE_DES_CBC));
    }

    #[test]
    fn a_list_with_nothing_usable_is_refused_not_replaced() {
        // everything below WARN, or only what russh can't do above it
        for (key, table) in [("Cipher", CIPHERS), ("KEX", KEXES), ("HostKey", HOST_KEYS)] {
            let all_below = std::iter::once("WARN").chain(table.iter().map(|p| p.0).filter(|n| *n != "WARN")).collect::<Vec<_>>().join(",");
            let e = preferred(&with(key, &all_below), |_| false).unwrap_err();
            assert!(e.to_string().contains("warn below here"), "{key}={all_below}: {e}");
        }
        let e = preferred(&with("Cipher", "blowfish,WARN"), |_| false).unwrap_err();
        assert!(e.to_string().contains("Connection > SSH > Cipher"), "{e}");
        // no saved list, or an empty one, is PuTTY's default, which is fine
        preferred(&Settings::default(), |_| false).unwrap();
        preferred(&with("Cipher", ""), |_| false).unwrap();
    }

    #[test]
    fn an_older_list_gets_newer_algorithms_as_putty_does() {
        // saved before PuTTY had ChaCha20 or AES-GCM: both go after AES, above WARN
        let p = preferred(&with("Cipher", "aes,3des,WARN,des"), |_| false).unwrap();
        assert!(p.cipher.contains(&cipher::CHACHA20_POLY1305));
        assert!(p.cipher.contains(&cipher::AES_256_GCM));
        let at = |c: cipher::Name| p.cipher.iter().position(|x| *x == c).unwrap();
        assert!(at(cipher::AES_256_GCM) < at(cipher::TRIPLE_DES_CBC));
        // ECDH and the hybrids go to the top of a key exchange list that lacks them
        let p = preferred(&with("KEX", "dh-gex-sha1,WARN"), |_| false).unwrap();
        assert_eq!(p.kex[0], kex::CURVE25519);
        assert!(p.kex.contains(&kex::MLKEM768X25519_SHA256));
    }

    #[test]
    fn a_kex_list_keeps_the_strict_kex_extension() {
        let p = preferred(&with("KEX", "ecdh,WARN,dh-group1-sha1"), |_| false).unwrap();
        // the hybrids aren't named, so PuTTY puts them first
        assert_eq!(p.kex[0], kex::MLKEM768X25519_SHA256);
        assert!(p.kex.contains(&kex::CURVE25519));
        assert!(p.kex.contains(&kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT));
        assert!(!p.kex.contains(&kex::DH_G1_SHA1));
    }

    #[test]
    fn known_host_key_types_go_first() {
        let p = preferred(&Settings::default(), |k| matches!(k, Algorithm::Rsa { .. })).unwrap();
        assert!(matches!(p.key[0], Algorithm::Rsa { .. }));
        let mut s = Settings::default();
        s.set("PreferKnownHostKeys", Value::Int(0));
        let p = preferred(&s, |k| matches!(k, Algorithm::Rsa { .. })).unwrap();
        assert_eq!(p.key[0], Algorithm::Ed25519);
    }

    #[test]
    fn rekey_sizes_parse_and_stay_inside_the_safety_limit() {
        assert_eq!(rekey_bytes(&with("RekeyBytes", "100K")), 102_400);
        assert_eq!(rekey_bytes(&with("RekeyBytes", "2M")), 2 << 20);
        assert_eq!(rekey_bytes(&with("RekeyBytes", "5G")), 1 << 30);
        assert_eq!(rekey_bytes(&with("RekeyBytes", "0")), 1 << 30);
    }

    #[test]
    fn terminal_modes_follow_the_three_forms() {
        let modes = terminal_modes(&with("TerminalModes", "INTR=V^C,ERASE=A,ECHO=Vno,QUIT=N,KILL=A"));
        assert!(modes.contains(&(Pty::VINTR, 3)));
        assert!(modes.contains(&(Pty::VERASE, 127)));
        assert!(modes.contains(&(Pty::ECHO, 0)));
        assert!(!modes.iter().any(|(p, _)| *p == Pty::VQUIT || *p == Pty::VKILL));
    }

    #[test]
    fn backspace_setting_reaches_the_erase_mode() {
        let mut s = Settings::default();
        s.set("BackspaceIsDelete", Value::Int(0));
        assert!(terminal_modes(&s).contains(&(Pty::VERASE, 8)));
    }
}

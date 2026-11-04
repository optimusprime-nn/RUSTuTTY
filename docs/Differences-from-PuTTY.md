# Differences from PuTTY

RUSTuTTY aims to do everything PuTTY does. Every one of PuTTY's saved settings
is listed in `crates/deck-core/src/options.rs` as working, not applicable, or
not done with the reason, and a test checks that list against PuTTY's own. A
setting that isn't done is still kept, so a saved session loses nothing.

## What it adds

- All sessions in one window, listed down the left, with keyboard shortcuts to
  move between them
- Paste to all sessions at once
- Dark and light themes
- A resizable settings window, and the main window's size kept between runs

## What isn't done

These can't be done with the libraries RUSTuTTY is built on:

- **GSSAPI and Kerberos login, SSH-1 and single DES.** russh, the SSH library,
  doesn't have them. A session set to SSH-1 only is refused rather than
  connected over SSH-2.
- **Workarounds for old servers' bugs.** russh makes those choices itself, and
  may refuse very old servers.

The tools differ too:

- `puttygen` is command line only. There's no key generator window.
- **Help** opens this documentation in your browser, rather than a help file.

## Different on purpose, for safety

- Every session keeps at least 10,000 lines of scrollback. PuTTY's saved
  sessions usually ask for 2000, which a long install or log outgrows, so its
  start would be lost. A session that asks for more gets more.

- Pasting anything with a line end asks first, even a single line copied with
  its newline, unless the program is using bracketed paste. **Warn before
  pasting line ends** under Window > Behaviour turns this off.
- An HTTP proxy only gets a password if it offers Basic or Digest login, and
  Basic shows a warning, because it sends the password readably.
- `pscp -scp` refuses files other than the one asked for. Wildcards need
  `-unsafe`.
- A proxy or pre-connect command won't run if a host name, user name or
  password it uses has a character `cmd.exe` would act on.
- No tool takes `-pw`: a password on a command line can be read by other
  programs.
- Pageant is reached by its window message, not its named pipe.
- `puttygen` asks for a passphrase on a new key and keeps a converted key's.
  `--no-passphrase` writes one without.
- Session logs hold what the host sent, not this end's prompts.
- Hosts can't hold the display back with synchronized updates.

## Different for other reasons

- Algorithms PuTTY lists below "warn below here" aren't offered at all, because
  russh has nowhere to stop and ask. If that leaves a session no cipher, key
  exchange or host key type RUSTuTTY can use, it isn't connected, and the
  message says which list to change.
- `Ctrl+Shift+C` and `Ctrl+Shift+V` always work, whatever **CtrlShiftCV** says.
- Connection sharing works between sessions in one RUSTuTTY. It can't join a
  connection held by a separate PuTTY, and forwardings on a joining session
  aren't set up.


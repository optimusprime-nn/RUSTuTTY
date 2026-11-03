# Building and testing

## What it's made of

| Crate | Holds |
| --- | --- |
| `deck-core` | Protocols, settings, keys, proxies and logging: everything but the window |
| `deck-ui` | The theme: colours, fonts, and drawing PuTTY's controls in the modern style |
| `rustutty` | The window, the session list and the settings window |
| `deck-tools` | Plink, PSCP, PSFTP, PuTTYgen and Pageant |
| `deck-setup` | The installer, which carries the programs inside it |

SSH and its cryptography come from [russh](https://crates.io/crates/russh), the
terminal from [alacritty_terminal](https://crates.io/crates/alacritty_terminal),
and the rest is written here, following PuTTY's source and file formats.

## Building

You need Rust and the Visual Studio 2022 Build Tools with the C++ workload.

    build.cmd

The programs and the installer are put in `dist`. For a release build that
includes third-party licence notices, run this first:

    python scripts/notices.py

## Testing

From a Visual Studio x64 developer prompt:

    cargo test -p deck-core -p deck-tools
    cargo test -p rustutty --features test-hooks
    cargo test -p deck-core --test sftp -- --ignored --test-threads=1

The first two run real servers on localhost. The window tests drive a special
build that opens off-screen, can't take focus and has a private clipboard, so
they're safe to run while you use the PC.

The third has the real PuTTY's `plink.exe` log in with key files and a Pageant
from here, so it needs PuTTY installed, the debug tools built, and no Pageant
running.

GitHub runs the first two on every push and pull request.

The **Audit** workflow checks `Cargo.lock` against the
[RustSec advisory database](https://rustsec.org) whenever the dependencies
change, and every day. To run it yourself:

    cargo install cargo-audit
    cargo audit --deny warnings

An advisory accepted for now, with the reason, goes in `.cargo/audit.toml`.

## Following PuTTY

The **Watch PuTTY** workflow checks PuTTY's main branch every day and lists new
commits in an issue labelled `putty-upstream`. Each one is reviewed, and what
applies is ported by hand, so security fixes and new features follow PuTTY
while the code stays a separate rewrite. `.putty-reviewed` holds the last
commit reviewed: update it when the issue is dealt with.

## Releasing

1. Set the version in `crates/rustutty`, `crates/deck-tools` and
   `crates/deck-setup`.
2. Push a tag that matches, such as `v1.0.0`.

The **Release** workflow tests everything, builds it, and publishes the
installer, the portable zip and `SHA256SUMS.txt`. A version with a hyphen, such
as `v1.0.0-beta.2`, is published as a pre-release. Running the workflow by hand
does everything but publish, and leaves the files on the run to try.

## Documentation

These pages live in `docs` in the repository. Pushing a change there updates
both the [website](https://optimusprime-nn.github.io/RUSTuTTY/) and the
[wiki](https://github.com/optimusprime-nn/RUSTuTTY/wiki): edit them in `docs`,
not in the wiki, or the next push will overwrite your change.

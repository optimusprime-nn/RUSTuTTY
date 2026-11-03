# Tools

RUSTuTTY comes with Rust versions of PuTTY's command-line tools. They have the
same names and take the same common options, so scripts written for PuTTY's
tools keep working.

| Tool | What it does |
| --- | --- |
| [Plink](#plink) | Runs a command or a shell on a server from a console, or holds tunnels open |
| [PSCP](#pscp) | Copies files to and from a server |
| [PSFTP](#psftp) | An interactive file transfer client |
| [PuTTYgen](#puttygen) | Makes and converts keys |
| [Pageant](#pageant) | Holds your keys so you only unlock them once |

They read the same saved sessions and trusted host keys as the main window.

## Options for connecting

Plink, PSCP and PSFTP all take these:

| Option | Meaning |
| --- | --- |
| `-ssh`, `-telnet`, `-rlogin`, `-raw`, `-serial` | The protocol, for Plink. PSCP and PSFTP always use SSH |
| `-P port` | The port |
| `-l user` | The user name |
| `-i key` | A private key file |
| `-load "name"` | Use a saved session's settings |
| `-L`, `-R`, `-D` | Port forwarding, as on the [command line](Command-Line.md) |
| `-N` | Don't start a shell or command: just forward ports |
| `-T` / `-t` | Don't / do ask for a terminal, whatever a saved session says |
| `-C` | Compress the connection |
| `-A` / `-a` | Allow / don't allow agent forwarding, whatever a saved session says |
| `-batch` | Never ask a question: fail instead, for scripts |

There's no `-pw`: a password on a command line can be read by other programs.

## Plink

    plink [options] [user@]host [command]

With a command, Plink runs it on the server and shows its output. Without one,
it gives you a shell in the console. A command gets no terminal unless you add
`-t`, as with OpenSSH's `ssh`.

Plink's own exit code doesn't yet pass on the remote command's: a script that
needs to know whether the command worked should check its output.

    plink -batch admin@server uptime
    plink -N -L 5432:db:5432 user@gateway

## PSCP

    pscp [options] [-r] [-sftp|-scp] [-unsafe] source... destination

A remote path is written `[user@]host:path`. A single letter before the colon is
a Windows drive, so `C:\file` is always local.

| Option | Meaning |
| --- | --- |
| `-r` | Copy folders and everything in them |
| `-sftp` | Use SFTP only |
| `-scp` | Use the old SCP protocol |
| `-unsafe` | Allow a server to send files other than the ones named, for wildcards over SCP |

PSCP uses SFTP, and falls back to SCP if the server has no SFTP.

Downloads never go through a link or junction inside the destination, since
the server chooses the names there. A name that leads to one stops the copy.
The destination you give may itself be a link.

    pscp report.pdf user@server:/srv/reports/
    pscp -r user@server:/var/log/app C:\logs

## PSFTP

    psftp [options] [user@]host [-b batchfile]

Type commands at the `psftp>` prompt, or put them in a file and pass it with
`-b`. Use double quotes around names with spaces.

| Command | Does |
| --- | --- |
| `ls`, `dir` | List the remote folder |
| `cd`, `pwd` | Change or show the remote folder |
| `lcd`, `lpwd` | Change or show the local folder |
| `get remote [local]` | Download a file |
| `put local [remote]` | Upload a file |
| `rm`, `del` | Delete a remote file |
| `mkdir`, `rmdir` | Make or remove a remote folder |
| `mv`, `ren` | Rename or move a remote file |
| `chmod` | Change a remote file's permissions |
| `help`, `?` | List the commands |
| `quit`, `bye`, `exit` | Leave |

## PuTTYgen

PuTTYgen here is command line only, like PuTTY's `puttygen` on Linux. There's
no window.

| To | Run |
| --- | --- |
| Make an Ed25519 key | `puttygen -t ed25519 -C "my laptop" -o key.ppk` |
| Make an RSA key | `puttygen -t rsa -b 4096 -o key.ppk` |
| Make an ECDSA key | `puttygen -t ecdsa -b 384 -o key.ppk` |
| Show the public key, for `authorized_keys` | `puttygen key.ppk -L` |
| Show the fingerprint | `puttygen key.ppk -l` |
| Change the passphrase | `puttygen key.ppk -P -o new.ppk` |
| Convert OpenSSH to PuTTY | `puttygen id_ed25519 -o key.ppk` |
| Convert PuTTY to OpenSSH | `puttygen key.ppk -O private-openssh -o id_ed25519` |

A new key asks for a passphrase. A converted key keeps the one it had unless
you give `-P` for a new one. To write a key with no passphrase on purpose, add
`--no-passphrase`. PuTTYgen won't overwrite a file: the output must not exist.

RSA keys must be at least 2048 bits. ECDSA keys are 256, 384 or 521 bits.

## Pageant

    pageant key.ppk [more keys]

Pageant asks for each key's passphrase in its console, then keeps the keys
unlocked for RUSTuTTY, PuTTY, and any other program that uses Pageant. Leave
the console open while you need them. Closing it, or pressing `Ctrl+C`, forgets
the keys.

There's no tray icon. Only programs running as you can use the keys.

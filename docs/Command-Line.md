# Command line

`rustutty.exe` takes PuTTY's main options, so shortcuts and scripts written for
PuTTY work. With no options, it opens with no session.

    rustutty [options] [user@]host[:port]

| Option | Meaning |
| --- | --- |
| `-ssh`, `-telnet`, `-rlogin`, `-raw`, `-serial` | The protocol. SSH unless the host says otherwise |
| `-P port` | The port |
| `-l user` | The user name |
| `-i key` | A private key file, `.ppk` or OpenSSH |
| `-load "name"` | Open a saved session |
| `-L [addr:]port:host:port` | Forward a local port to a host the server can reach |
| `-R [addr:]port:host:port` | Forward a port on the server back to a host here |
| `-D [addr:]port` | A SOCKS proxy on a local port, through the server |

The host can also be a URL, such as `telnet://host` or `serial://COM3`; see
[Getting started](Getting-Started.md#your-first-connection).

## Examples

    rustutty -ssh admin@192.0.2.10
    rustutty -load "web server"
    rustutty -i C:\keys\deploy.ppk -L 8080:intranet:80 -D 1080 user@gateway
    rustutty serial://COM3

Each run opens its own window. For several sessions in one window, open them
from the window you already have.

## Passwords

There's no `-pw` option. A password on a command line can be read by other
programs on the PC. Use a key, Pageant, or type the password when asked.

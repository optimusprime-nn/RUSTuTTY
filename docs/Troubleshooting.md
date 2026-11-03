# Troubleshooting

## Windows protected your PC

The installer isn't code-signed yet. Choose **More info**, then **Run anyway**.
To be sure the file is the one released, check it against `SHA256SUMS.txt`, as
described in [Installing](Installing.md#checking-a-download).

## My PuTTY sessions aren't listed

RUSTuTTY reads sessions from `HKCU\Software\SimonTatham\PuTTY\Sessions`, which
is where PuTTY's installer and its usual zip keep them. A PuTTY that saves to a
file instead, such as some portable builds, isn't read. Save the session again
in RUSTuTTY.

## A server is refused, or the connection drops at once

Open the **Event Log** from the window menu. It shows how far the connection
got, and its last line says why it ended. RUSTuTTY doesn't offer SSH-1,
single DES, or the algorithms PuTTY lists below "warn below here", and very old
servers may have nothing in common with it. See
[Differences from PuTTY](Differences-from-PuTTY.md).

## The host key warning appears for a server I trust

RUSTuTTY trusts the keys PuTTY trusts and the ones you accept here. If a
server's key has really changed, accept the new one. If you didn't expect it to
change, don't: that's the warning doing its job.

## Pasting asks before it pastes

That's the paste warning, which stops a stray paste from running commands.
It asks whenever the text has a line end, even just one at the end, since that
presses Enter. Untick **Warn before pasting line ends** under Window > Behaviour in
[Settings](Settings.md) if you'd rather it didn't.

## Ctrl+C or Ctrl+V doesn't do what I expect

`Ctrl+C` copies only when text is selected; otherwise it interrupts. In
full-screen programs `Ctrl+V` goes to the program; `Ctrl+Shift+V` always
pastes. See [The window](The-Window.md#copy-and-paste).

## I can't select text

The program on the server is using the mouse. Hold **Shift** while you select.

## The window opens at an odd size

It reopens at the size it was when last closed. Resize it and close it, or
untick **Save window size on exit** under Window > Behaviour.

## Reporting a problem

Open an issue at
[github.com/optimusprime-nn/RUSTuTTY/issues](https://github.com/optimusprime-nn/RUSTuTTY/issues).
Say what you did, what you expected, and what happened, with the version from
**About RUSTuTTY**. The Event Log helps with connection problems, but take out
host names, user names and anything else private before posting it.

# Installing

RUSTuTTY runs on 64-bit Windows 10 and 11. Download it from the
[Releases page](https://github.com/optimusprime-nn/RUSTuTTY/releases). Each
release has two downloads, which hold the same programs:

| Download | What it does |
| --- | --- |
| `RUSTuTTY-<version>-setup.exe` | Installs for you only, adds a Start menu entry, and can be removed in Settings > Apps |
| `RUSTuTTY-<version>-portable.zip` | Nothing to install: unzip it anywhere, including a USB stick, and run `rustutty.exe` |

## The installer

Run `RUSTuTTY-<version>-setup.exe` and confirm. It needs no administrator
rights, because it installs into your own profile:

    %LOCALAPPDATA%\Programs\RUSTuTTY

It puts `rustutty.exe`, `plink.exe`, `pscp.exe`, `psftp.exe`, `puttygen.exe`,
`pageant.exe` and `LICENCE.txt` there, and adds RUSTuTTY to the Start menu.
Running a newer setup over an older one upgrades it in place.

For scripted installs:

    RUSTuTTY-<version>-setup.exe /S

installs silently.

### SmartScreen

The installer isn't code-signed yet, so Windows SmartScreen may say it
"protected your PC". Choose **More info**, then **Run anyway**. If you'd rather
check the file first, see below.

## Checking a download

Each release has a `SHA256SUMS.txt`. In PowerShell:

    Get-FileHash .\RUSTuTTY-<version>-setup.exe

The hash it prints should match the line for that file in `SHA256SUMS.txt`
(the file uses lower case, PowerShell upper case).

## Removing it

Use **Settings > Apps > Installed apps > RUSTuTTY > Uninstall**, or run
`uninstall.exe /U` from the install folder. `uninstall.exe /U /S` removes it
silently.

Removing RUSTuTTY leaves your saved sessions and host keys alone. Those saved by
PuTTY belong to PuTTY, and those saved by RUSTuTTY stay in
`HKCU\Software\PuTTYDeck` in case you install it again. To remove those too,
delete that registry key.

## Using it next to PuTTY

RUSTuTTY and PuTTY can both be installed. RUSTuTTY reads PuTTY's saved sessions
and trusted host keys but never changes them, so PuTTY carries on exactly as
before. See [Settings](Settings.md#where-settings-are-kept).

The command-line tools have the same names as PuTTY's. If both folders are on
your `PATH`, the one listed first wins.

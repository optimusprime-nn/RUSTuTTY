# RUSTuTTY's copy of alacritty_terminal

This is alacritty_terminal 0.26.0 from crates.io, Apache-2.0 (see
LICENSE-APACHE), with these changes for what PuTTY does:

- `Flags::BLINK`, set and cleared by SGR 5, 6 and 25, so blinking text can
  be shown (PuTTY's `BlinkText`).
- `Config::bce` and `Grid::bce`: with background colour erase off, cleared
  and scrolled-in cells are plain default cells (PuTTY's `BCE`).
- `Config::cjk_ambiguous_wide`: East Asian ambiguous-width characters take
  two cells (PuTTY's `CJKAmbigWide`).

Each change is marked with a `RUSTuTTY` comment.

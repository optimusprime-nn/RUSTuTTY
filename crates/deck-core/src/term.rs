//! The terminal: alacritty's emulator, plus what the deck needs from it.

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{ClearMode, Color, CursorShape, CursorStyle, Handler, NamedColor, Processor};
use crate::filter::{Action, Filter, FilterConfig, ERASE_IN_PLACE};
use encoding_rs::{Decoder, Encoder, Encoding, UTF_8};
use crate::settings::Settings;
use std::sync::{Arc, Mutex};

pub type Rgb = (u8, u8, u8);

/// PuTTY's 22 colours, in the order of its `Colour0` to `Colour21` settings.
/// The longest title shown, in characters.
const MAX_TITLE: usize = 512;

const DEFAULT_COLOURS: [Rgb; 22] = [
    (187, 187, 187), (255, 255, 255), (0, 0, 0), (85, 85, 85), (0, 0, 0), (0, 255, 0),
    (0, 0, 0), (85, 85, 85), (187, 0, 0), (255, 85, 85), (0, 187, 0), (85, 255, 85),
    (187, 187, 0), (255, 255, 85), (0, 0, 187), (85, 85, 255), (187, 0, 187), (255, 85, 255),
    (0, 187, 187), (85, 255, 255), (187, 187, 187), (255, 255, 255),
];

/// A session's colours, and how bold is shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    fg: Rgb,
    bold_fg: Rgb,
    bg: Rgb,
    pub cursor: Rgb,
    ansi: [Rgb; 16],
    bold_as_colour: bool,
    /// Whether bold text also uses the bold font (`BoldAsColour` 0 or 2).
    pub bold_font: bool,
}

impl Palette {
    pub fn from_settings(s: &Settings) -> Palette {
        let mut c = DEFAULT_COLOURS;
        for (i, slot) in c.iter_mut().enumerate() {
            let text = s.s(&format!("Colour{i}"), "");
            let parts: Vec<u8> = text.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            if let [r, g, b] = parts[..] {
                *slot = (r, g, b);
            }
        }
        // PuTTY keeps each normal colour next to its bold form
        let mut ansi = [(0, 0, 0); 16];
        for i in 0..8 {
            ansi[i] = c[6 + i * 2];
            ansi[i + 8] = c[7 + i * 2];
        }
        Palette { fg: c[0], bold_fg: c[1], bg: c[2], cursor: c[5], ansi, bold_as_colour: s.i("BoldAsColour", 1) != 0, bold_font: s.i("BoldAsColour", 1) != 1 }
    }

    /// `UseSystemColours`: Windows's own text and window colours.
    pub fn use_system(&mut self, fg: Rgb, bg: Rgb) {
        self.fg = fg;
        self.bold_fg = fg;
        self.bg = bg;
    }

    pub fn background(&self) -> Rgb {
        self.bg
    }

    fn indexed(&self, i: u8) -> Rgb {
        match i {
            0..=15 => self.ansi[i as usize],
            _ => cube_or_grey(i),
        }
    }

    /// The background a blinking cell gets with `BlinkText` off: the bright
    /// version of one of the eight colours, as PuTTY's, or as it was.
    fn brighter_background(&self, c: Color, now: Rgb, host: &Colors) -> Rgb {
        match c {
            Color::Indexed(i) if i < 8 => self.entry(i + 8, host),
            Color::Named(n) if (n as usize) < 8 => self.entry(n as u8 + 8, host),
            _ => now,
        }
    }

    /// A palette entry, or the colour the host gave it with OSC 4 (or Linux's
    /// OSC P), as PuTTY lets a host change its palette.
    fn entry(&self, i: u8, host: &Colors) -> Rgb {
        host[i as usize].map_or_else(|| self.indexed(i), |c| (c.r, c.g, c.b))
    }

    fn resolve(&self, c: Color, bold: bool, default: Rgb, host: &Colors) -> Rgb {
        let brighten = bold && self.bold_as_colour;
        match c {
            Color::Spec(c) => (c.r, c.g, c.b),
            Color::Indexed(i) if brighten && i < 8 => self.entry(i + 8, host),
            Color::Indexed(i) => self.entry(i, host),
            Color::Named(n) => {
                let i = n as usize;
                if i < 16 {
                    self.entry(if brighten && i < 8 { i + 8 } else { i } as u8, host)
                } else if n == NamedColor::Background {
                    self.bg
                } else {
                    default
                }
            }
        }
    }

    /// The colour a query for entry `i` is answered with, as the terminal
    /// library numbers them: the 256-colour palette, then the default text
    /// and background, then the cursor.
    fn queried(&self, i: usize, host: &Colors) -> Rgb {
        match i {
            0..=255 => self.entry(i as u8, host),
            256 => self.fg,
            257 => self.bg,
            _ => self.cursor,
        }
    }
}

fn cube_or_grey(i: u8) -> Rgb {
    match i {
        16..=231 => {
            let i = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (step(i / 36), step((i / 6) % 6), step(i % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderCell {
    pub c: char,
    /// Combining characters drawn over `c`, such as accents, `'\0'` where
    /// unused. PuTTY keeps them with the character before them too.
    pub marks: [char; MARKS],
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
    pub strike: bool,
    /// Blinking text, with `BlinkText` on: drawn, then hidden, in turn.
    pub blink: bool,
    pub wide: bool,
}

/// The most combining characters drawn on one cell. More are rare, and stay
/// in the text for copying.
pub const MARKS: usize = 3;

pub struct Screen {
    pub rows: Vec<Vec<RenderCell>>,
    pub cursor: Option<(usize, usize)>,
}

/// What the emulator wants done, collected for the owner to act on.
#[derive(Debug, PartialEq)]
pub enum TermEvent {
    /// Bytes to send back to the host, such as a reply to a query.
    Reply(Vec<u8>),
    Title(String),
    Bell,
    Print(Vec<u8>),
    PrintEnd,
    /// Reverse video on and straight off again: PuTTY makes sure that shows,
    /// as a visual bell does.
    Flash,
    /// An xterm window operation for the window to do or answer, its numbers
    /// as the host sent them.
    WindowOp(Vec<u32>),
}

/// How long reverse video turned on and off must show for, as PuTTY's
/// `VBELL_DELAY`.
const FLASH_FOR: std::time::Duration = std::time::Duration::from_millis(100);

/// `PasteRTF`: text as rich text in a font, for pasting into a word processor.
pub fn rtf(text: &str, font: &str, points: u32) -> String {
    let safe_font: String = font.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect();
    let mut out = format!("{{\\rtf1\\ansi\\deff0{{\\fonttbl{{\\f0\\fmodern {safe_font};}}}}\\f0\\fs{} ", points * 2);
    for c in text.chars() {
        match c {
            '\\' | '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out += "\\par\n",
            '\r' => {}
            c if (c as u32) < 0x80 => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    out += &format!("\\u{}?", *u as i16);
                }
            }
        }
    }
    out.push('}');
    out
}

/// A run of selected text in one look, for rich-text copying.
#[derive(Clone, Debug, PartialEq)]
pub struct Styled {
    pub text: String,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub underline: bool,
}

/// `PasteRTF` with the selection's colours, bold and underline, as PuTTY
/// writes it: a colour table, then `\cf` for text and `\highlight` for the
/// background.
pub fn rtf_styled(runs: &[Styled], font: &str, points: u32) -> String {
    let mut colours: Vec<Rgb> = Vec::new();
    let mut index = |c: Rgb| match colours.iter().position(|&k| k == c) {
        Some(i) => i + 1,
        None => {
            colours.push(c);
            colours.len()
        }
    };
    let mut body = String::new();
    for r in runs {
        let (f, b) = (index(r.fg), index(r.bg));
        body += &format!("{{\\cf{f}\\highlight{b}\\chcbpat{b}{}{} ", if r.bold { "\\b" } else { "" }, if r.underline { "\\ul" } else { "" });
        // the text as plain rtf() writes it, without its own header
        let plain = rtf(&r.text, "", 0);
        let inner = plain.split_once("\\fs0 ").map_or("", |(_, rest)| rest);
        body += inner.strip_suffix('}').unwrap_or(inner);
        body.push('}');
    }
    let table: String = colours.iter().map(|c| format!("\\red{}\\green{}\\blue{};", c.0, c.1, c.2)).collect();
    let safe_font: String = font.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').collect();
    format!("{{\\rtf1\\ansi\\deff0{{\\fonttbl{{\\f0\\fmodern {safe_font};}}}}{{\\colortbl ;{table}}}\\f0\\fs{} {body}}}", points * 2)
}

/// A host asking what colour a palette entry is, and how to word the answer.
type ColourQuery = (usize, Arc<dyn Fn(alacritty_terminal::vte::ansi::Rgb) -> String + Send + Sync>);

/// What the terminal library raises: events, and colour queries, which wait
/// for the palette in `Terminal::feed` to be answered.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<TermEvent>>>, Arc<Mutex<Vec<ColourQuery>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let e = match event {
            Event::PtyWrite(s) => TermEvent::Reply(s.into_bytes()),
            Event::Title(t) => TermEvent::Title(clean_title(&t)),
            Event::Bell => TermEvent::Bell,
            Event::ColorRequest(i, answer) => {
                self.1.lock().unwrap().push((i, answer));
                return;
            }
            _ => return,
        };
        self.0.lock().unwrap().push(e);
    }
}

pub struct Terminal {
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    anchor: Option<Point>,
    dragged: bool,
    kind: SelectionType,
    palette: Palette,
    mouse_reports: bool,
    /// `BlinkText`: blinking text blinks.
    blink_text: bool,
    /// `DisableBidi`: right-to-left text is shown in the order it came.
    no_bidi: bool,
    /// `DisableArabicShaping`: Arabic letters keep their plain forms.
    no_shaping: bool,
    bracketed_paste: bool,
    /// The session's starting modes, put back by a reset.
    start: Vec<u8>,
    /// `NoRemoteWinTitle` off: the host may set the title, and a title query
    /// then reports its title rather than the session's own.
    remote_title: bool,
    /// Mode 1015: mouse reports in urxvt's form.
    urxvt_mouse: bool,
    /// DECSCNM: the whole screen in reverse video, since when.
    reverse: Option<std::time::Instant>,
    filter: Filter,
    decoder: Option<Decoder>,
    /// The session's own character set, which `ESC % @` returns to.
    codepage: &'static Encoding,
    /// The start of an escape at the end of the last output, kept for the next.
    raw_tail: Vec<u8>,
    encoder: Option<Encoder>,
    title: String,
    scroll_on_key: bool,
    paste_controls: bool,
    rect_select: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CursorKind {
    Block,
    Underline,
    Bar,
}

fn cursor_style(s: &Settings) -> CursorStyle {
    let shape = match s.i("CurType", 0) {
        1 => CursorShape::Underline,
        2 => CursorShape::Beam,
        _ => CursorShape::Block,
    };
    CursorStyle { shape, blinking: s.b("BlinkCur", false) }
}

/// A title can't be allowed to end the report it's sent back in, or to reorder
/// itself into something else in the session list.
fn clean_title(t: &str) -> String {
    t.chars().filter(|c| !c.is_control() && !crate::session::is_direction_mark(*c)).take(MAX_TITLE).collect()
}

/// `LineCodePage` is PuTTY's description of a character set, such as
/// "ISO-8859-1:1998 (Latin-1, West Europe)" or "Win1252 (Western)". Anything
/// not recognised is treated as UTF-8.
pub(crate) fn line_encoding(name: &str) -> &'static Encoding {
    let label = name.split([':', ' ']).next().unwrap_or("").to_lowercase();
    let label = match label.strip_prefix("win") {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => format!("windows-{rest}"),
        _ => label,
    };
    Encoding::for_label(label.as_bytes()).unwrap_or(UTF_8)
}

/// PuTTY's default character classes for double-click selection, 32 to a
/// line from character 0, as settings.c has them: 0 is space, 1 punctuation
/// and 2 part of a word.
const WORDNESS: [&str; 8] = [
    "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0",
    "0,1,2,1,1,1,1,1,1,1,1,1,1,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,1,1",
    "1,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,2",
    "1,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,1,1,1,1",
    "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
    "1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1",
    "2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,2,2,2,2,2,2,2,2",
    "2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,1,2,2,2,2,2,2,2,2",
];

/// The characters a double click stops at, from the `Wordness` settings.
/// The terminal library knows only word characters and breaks, so every
/// character PuTTY doesn't class as part of a word is a break.
fn word_breaks(s: &Settings) -> String {
    let mut breaks = String::new();
    for (block, default) in WORDNESS.iter().enumerate() {
        let saved = s.s(&format!("Wordness{}", block * 32), default);
        let classes: Vec<&str> = saved.split(',').collect();
        for (i, fallback) in default.split(',').enumerate() {
            let class = classes.get(i).map(|c| c.trim()).filter(|c| !c.is_empty()).unwrap_or(fallback);
            if class != "2" {
                if let Some(c) = char::from_u32((block * 32 + i) as u32) {
                    breaks.push(c);
                }
            }
        }
    }
    breaks
}

/// The fewest lines of scrollback a session keeps. PuTTY's saved sessions
/// usually ask for 2000, which a long install or log outgrows, losing its
/// start. A session can ask for more, not less.
pub const MIN_SCROLLBACK: u32 = 10_000;

/// `ScrollbackLines`, at least `MIN_SCROLLBACK`.
fn scrollback_lines(s: &Settings) -> usize {
    s.i("ScrollbackLines", MIN_SCROLLBACK).clamp(MIN_SCROLLBACK, 1_000_000) as usize
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Terminal {
        Terminal::with_settings(cols, rows, &Settings::default())
    }

    /// The terminal library's settings from the session's.
    fn config(s: &Settings) -> Config {
        Config {
            scrolling_history: scrollback_lines(s),
            // A remote host must not be able to touch the clipboard.
            osc52: Osc52::Disabled,
            default_cursor_style: cursor_style(s),
            bce: s.b("BCE", true),
            cjk_ambiguous_wide: s.b("CJKAmbigWide", false),
            semantic_escape_chars: word_breaks(s),
            ..Config::default()
        }
    }

    /// Change Settings on a running session, as PuTTY's `term_reconfig`: the
    /// new settings take over, and the screen, scrollback and modes the host
    /// has set stay.
    pub fn reconfigure(&mut self, s: &Settings) {
        self.term.set_options(Terminal::config(s));
        self.filter.set_config(FilterConfig::new(s));
        let encoding = line_encoding(&s.s("LineCodePage", ""));
        let now = self.encoder.as_ref().map_or(UTF_8, |e| e.encoding());
        self.codepage = encoding;
        if encoding != now {
            self.decoder = (encoding != UTF_8).then(|| encoding.new_decoder_without_bom_handling());
            self.encoder = (encoding != UTF_8).then(|| encoding.new_encoder());
        }
        self.remote_title = !s.b("NoRemoteWinTitle", false);
        if !self.remote_title {
            self.title = s.s("WinTitle", "");
        }
        self.scroll_on_key = s.b("ScrollOnKey", false);
        self.paste_controls = s.b("PasteControls", false);
        self.rect_select = s.b("RectSelect", false);
        self.palette = Palette::from_settings(s);
        self.mouse_reports = !s.b("NoMouseReporting", false);
        self.blink_text = s.b("BlinkText", false);
        self.no_bidi = s.b("DisableBidi", false);
        self.no_shaping = s.b("DisableArabicShaping", false);
        self.bracketed_paste = !s.b("DisableBracketedPaste", false);
    }

    pub fn with_settings(cols: usize, rows: usize, s: &Settings) -> Terminal {
        let events = Listener::default();
        let term = Term::new(Terminal::config(s), &TermSize::new(cols.max(1), rows.max(1)), events.clone());
        let encoding = line_encoding(&s.s("LineCodePage", ""));
        let mut t = Terminal {
            term,
            parser: Processor::new(),
            filter: Filter::new(FilterConfig::new(s)),
            decoder: (encoding != UTF_8).then(|| encoding.new_decoder_without_bom_handling()),
            encoder: (encoding != UTF_8).then(|| encoding.new_encoder()),
            codepage: encoding,
            raw_tail: Vec::new(),
            title: s.s("WinTitle", ""),
            remote_title: !s.b("NoRemoteWinTitle", false),
            urxvt_mouse: false,
            reverse: None,
            scroll_on_key: s.b("ScrollOnKey", false),
            paste_controls: s.b("PasteControls", false),
            rect_select: s.b("RectSelect", false),
            events,
            anchor: None,
            dragged: false,
            kind: SelectionType::Simple,
            palette: Palette::from_settings(s),
            mouse_reports: !s.b("NoMouseReporting", false),
            blink_text: s.b("BlinkText", false),
            no_bidi: s.b("DisableBidi", false),
            no_shaping: s.b("DisableArabicShaping", false),
            bracketed_paste: !s.b("DisableBracketedPaste", false),
            start: Vec::new(),
        };
        // the settings that are modes a host could also set later
        let mut start = Vec::new();
        for (key, default, sequence) in [
            ("AutoWrapMode", true, &b"\x1b[?7l"[..]),
            ("DECOriginMode", false, &b"\x1b[?6l"[..]),
            ("ApplicationCursorKeys", false, &b"\x1b[?1l"[..]),
            ("ApplicationKeypad", false, &b"\x1b>"[..]),
        ] {
            let on = s.b(key, default);
            if on != default {
                // a mode turned on differs only in its last byte
                let mut seq = sequence.to_vec();
                match seq.last_mut() {
                    Some(b @ b'l') if on => *b = b'h',
                    Some(b @ b'>') if on => *b = b'=',
                    _ => {}
                }
                start.extend(seq);
            }
        }
        t.parser.advance(&mut t.term, &start);
        t.start = start;
        t
    }

    /// Clear Scrollback from the menu. It goes straight to the terminal, not
    /// through the filter, which may be set to refuse a host asking the same.
    pub fn clear_scrollback(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.term.grid_mut().clear_history();
    }

    /// Reset Terminal from the menu: back to how the session started, with its
    /// own starting modes, not the terminal library's.
    pub fn reset(&mut self) {
        self.urxvt_mouse = false;
        self.reverse = None;
        self.parser.advance(&mut self.term, b"\x1bc");
        self.parser.advance(&mut self.term, &self.start);
    }

    /// Clears the screen without pushing it into the scrollback, for
    /// `EraseToScrollback` off. The cursor and the host's saved cursor stay.
    fn erase_in_place(&mut self) {
        let at = self.term.grid().cursor.point;
        self.term.grid_mut().cursor.point = Point::new(Line(0), Column(0));
        self.term.clear_screen(ClearMode::Below);
        self.term.grid_mut().cursor.point = at;
    }

    /// A soft reset for a restarted session, which keeps the screen.
    pub fn soft_reset(&mut self) {
        self.parser.advance(&mut self.term, b"\x1b[!p");
        self.parser.advance(&mut self.term, &self.start);
    }

    /// What the host sent. `ESC % G` and `ESC % @` switch to UTF-8 and back
    /// to the session's character set, as in PuTTY; they come before the
    /// bytes are decoded, so they're found here.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut data = std::mem::take(&mut self.raw_tail);
        data.extend_from_slice(bytes);
        // an escape cut off at the end waits for the rest
        let keep = if data.ends_with(b"\x1b%") { 2 } else if data.ends_with(b"\x1b") { 1 } else { 0 };
        self.raw_tail = data.split_off(data.len() - keep);
        let mut rest = &data[..];
        while let Some(at) = rest.windows(3).position(|w| w[0] == 0x1b && w[1] == b'%' && matches!(w[2], b'G' | b'@')) {
            self.feed_decoded(&rest[..at]);
            let encoding = if rest[at + 2] == b'G' { UTF_8 } else { self.codepage };
            self.decoder = (encoding != UTF_8).then(|| encoding.new_decoder_without_bom_handling());
            self.encoder = (encoding != UTF_8).then(|| encoding.new_encoder());
            rest = &rest[at + 3..];
        }
        self.feed_decoded(rest);
    }

    fn feed_decoded(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let decoded;
        let bytes = match &mut self.decoder {
            Some(d) => {
                let mut text = String::with_capacity(bytes.len() * 3 + 16);
                let _ = d.decode_to_string(bytes, &mut text, false);
                decoded = text;
                decoded.as_bytes()
            }
            None => bytes,
        };
        let mut actions = Vec::new();
        let filtered = self.filter.feed(bytes, &mut actions);
        // output that changes selected text ends the selection, as in PuTTY, so
        // a copy can't give text that's no longer on the screen
        let selected = self.term.selection.is_some().then(|| self.term.selection_to_string());
        let mut rest = &filtered[..];
        while let Some(at) = rest.windows(ERASE_IN_PLACE.len()).position(|w| w == ERASE_IN_PLACE) {
            self.parser.advance(&mut self.term, &rest[..at]);
            self.erase_in_place();
            rest = &rest[at + ERASE_IN_PLACE.len()..];
        }
        self.parser.advance(&mut self.term, rest);
        if selected.is_some_and(|before| self.term.selection_to_string() != before) {
            self.term.selection = None;
        }
        // colour queries, answered from the palette as it is now
        let queries = std::mem::take(&mut *self.events.1.lock().unwrap());
        for (i, answer) in queries {
            let (r, g, b) = self.palette.queried(i, self.term.colors());
            let reply = answer(alacritty_terminal::vte::ansi::Rgb { r, g, b });
            self.events.0.lock().unwrap().push(TermEvent::Reply(reply.into_bytes()));
        }
        for a in actions {
            let reply = match a {
                Action::Reply(b) => b,
                Action::ReportTitle(kind) => format!("\x1b]{}{}\x1b\\", kind as char, clean_title(&self.title)).into_bytes(),
                Action::WindowOp(numbers) => {
                    self.events.0.lock().unwrap().push(TermEvent::WindowOp(numbers));
                    continue;
                }
                Action::Print(b) => {
                    self.events.0.lock().unwrap().push(TermEvent::Print(b));
                    continue;
                }
                Action::PrintEnd => {
                    self.events.0.lock().unwrap().push(TermEvent::PrintEnd);
                    continue;
                }
                Action::UrxvtMouse(on) => {
                    self.urxvt_mouse = on;
                    continue;
                }
                Action::ReverseVideo(true) => {
                    self.reverse.get_or_insert_with(std::time::Instant::now);
                    continue;
                }
                Action::ReverseVideo(false) => {
                    if self.reverse.take().is_some_and(|since| since.elapsed() < FLASH_FOR) {
                        self.events.0.lock().unwrap().push(TermEvent::Flash);
                    }
                    continue;
                }
            };
            self.events.0.lock().unwrap().push(TermEvent::Reply(reply));
        }
    }

    /// Text typed or pasted, in the session's character set.
    pub fn encode_text(&mut self, text: &str) -> Vec<u8> {
        let Some(e) = &mut self.encoder else { return text.as_bytes().to_vec() };
        let mut out = Vec::with_capacity(text.len() * 2 + 8);
        let _ = e.encode_from_utf8_to_vec(text, &mut out, false);
        out
    }

    pub fn scroll_on_key(&self) -> bool {
        self.scroll_on_key
    }

    pub fn cursor_style(&self) -> (CursorKind, bool) {
        let style = self.term.cursor_style();
        let kind = match style.shape {
            CursorShape::Underline => CursorKind::Underline,
            CursorShape::Beam => CursorKind::Bar,
            _ => CursorKind::Block,
        };
        (kind, style.blinking)
    }

    /// Where the view is in the scrollback, for a scrollbar: lines of history,
    /// and how far up from the bottom the view is.
    pub fn scroll_position(&self) -> (usize, usize) {
        let g = self.term.grid();
        (g.history_size(), g.display_offset())
    }

    pub fn scroll_to(&mut self, from_bottom: usize) {
        let now = self.term.grid().display_offset() as i32;
        self.term.scroll_display(Scroll::Delta(from_bottom as i32 - now));
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        let events = std::mem::take(&mut *self.events.0.lock().unwrap());
        for e in &events {
            match e {
                TermEvent::Title(t) if self.remote_title => self.title = t.clone(),
                _ => {}
            }
        }
        events
    }

    pub fn size(&self) -> (usize, usize) {
        (self.term.columns(), self.term.screen_lines())
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        self.term.resize(TermSize::new(cols.max(1), rows.max(1)));
    }

    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }

    /// Whether a program has the mouse, so clicks go to it, not to selecting.
    pub fn wants_mouse(&self) -> bool {
        self.mouse_reports && self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// What a mouse event should be sent as, if the program asked for them,
    /// as PuTTY's `term_mouse`. `button` is 0 left, 1 middle, 2 right, 3 none
    /// (a move with no button held), 64 to 67 the wheel up, down, left and
    /// right. `mods` is Shift and Ctrl.
    pub fn mouse_report(&self, button: u8, col: usize, row: usize, pressed: bool, motion: bool, mods: (bool, bool)) -> Option<Vec<u8>> {
        let mode = *self.term.mode();
        if !self.mouse_reports {
            return None;
        }
        let wanted = match (motion, button) {
            // mode 1003 reports moves with no button held
            (true, 3) => mode.contains(TermMode::MOUSE_MOTION),
            (true, _) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
            (false, _) => mode.intersects(TermMode::MOUSE_MODE),
        };
        if !wanted {
            return None;
        }
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        // only 1006 can say which button was let go
        let code = if pressed || sgr { button } else { 3 };
        let code = code + if motion { 32 } else { 0 } + if mods.0 { 4 } else { 0 } + if mods.1 { 16 } else { 0 };
        let (x, y) = (col + 1, row + 1);
        if sgr {
            let end = if pressed { 'M' } else { 'm' };
            return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
        }
        if self.urxvt_mouse {
            return Some(format!("\x1b[{};{x};{y}M", code + 32).into_bytes());
        }
        // the old form can't say a place past 223, so says nothing, as PuTTY
        (x <= 223 && y <= 223).then(|| vec![0x1b, b'[', b'M', code + 32, (x + 32) as u8, (y + 32) as u8])
    }

    pub fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn app_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    fn point(&self, col: usize, row: usize) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let col = col.min(self.term.columns() - 1);
        Point::new(Line(row as i32 - offset), Column(col))
    }

    pub fn select_start(&mut self, col: usize, row: usize) {
        self.select_start_with(col, row, false);
    }

    /// `flip` is Alt, which swaps between line and rectangular selection.
    pub fn select_start_with(&mut self, col: usize, row: usize, flip: bool) {
        let p = self.point(col, row);
        self.anchor = Some(p);
        self.dragged = false;
        self.kind = if self.rect_select != flip { SelectionType::Block } else { SelectionType::Simple };
        self.term.selection = Some(Selection::new(self.kind, p, Side::Left));
    }

    /// Extends the selection from whichever end is nearer, for the mouse button
    /// PuTTY calls "extend".
    /// As PuTTY: the end of the selection nearer the pointer moves, and the
    /// other stays. A rectangle keeps its nearest corner's opposite. Words or
    /// lines stay words or lines.
    pub fn select_extend(&mut self, col: usize, row: usize) {
        let p = self.point(col, row);
        let range = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term));
        match range {
            Some(r) if self.kind == SelectionType::Block => {
                let line = if 2 * p.line.0 < r.start.line.0 + r.end.line.0 { r.end.line } else { r.start.line };
                let column = if 2 * p.column.0 < r.start.column.0 + r.end.column.0 { r.end.column } else { r.start.column };
                self.anchor = Some(Point::new(line, column));
            }
            Some(r) => {
                let cols = self.term.columns() as i64;
                let at = |q: Point| q.line.0 as i64 * cols + q.column.0 as i64;
                let near_start = at(p) - at(r.start) < (at(r.end) - at(r.start)) / 2;
                self.anchor = Some(if near_start { r.end } else { r.start });
            }
            None => self.select_start(col, row),
        }
        self.dragged = true;
        self.select_to(col, row);
    }

    /// Extends a selection. Staying in the first cell selects nothing, so a
    /// click with a slight wobble can't overwrite the clipboard.
    pub fn select_to(&mut self, col: usize, row: usize) {
        let Some(anchor) = self.anchor else { return };
        let p = self.point(col, row);
        self.dragged |= p != anchor;
        if !self.dragged {
            return;
        }
        // both end cells count, whichever way the drag goes
        let (from, to) = if p < anchor { (Side::Right, Side::Left) } else { (Side::Left, Side::Right) };
        let mut sel = Selection::new(self.kind, anchor, from);
        sel.update(p, to);
        self.term.selection = Some(sel);
    }

    /// A double click. Dragging on from here extends by whole words.
    pub fn select_word(&mut self, col: usize, row: usize) {
        self.select_whole(SelectionType::Semantic, col, row);
    }

    /// A triple click. Dragging on from here extends by whole lines.
    pub fn select_line(&mut self, row: usize) {
        self.select_whole(SelectionType::Lines, 0, row);
    }

    fn select_whole(&mut self, kind: SelectionType, col: usize, row: usize) {
        let p = self.point(col, row);
        self.anchor = Some(p);
        self.kind = kind;
        self.dragged = true;
        self.term.selection = Some(Selection::new(kind, p, Side::Left));
    }

    pub fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|s| !s.is_empty())
    }

    pub fn selected_text(&self) -> Option<String> {
        if !self.has_selection() {
            return None;
        }
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The selection as runs of text in one look, for rich-text copying, as
    /// PuTTY's: colours, bold and underline. Lines end where the screen's do,
    /// not where a long line wrapped, and trailing blanks go, as in plain
    /// copying.
    pub fn selected_styled(&self) -> Vec<Styled> {
        let Some(range) = self.term.selection.as_ref().and_then(|s| s.to_range(&self.term)) else { return Vec::new() };
        let grid = self.term.grid();
        let p = &self.palette;
        let colors = self.term.colors();
        let last = self.term.columns() - 1;
        let mut runs: Vec<Styled> = Vec::new();
        let mut push = |text: &str, fg: Rgb, bg: Rgb, bold: bool, underline: bool| match runs.last_mut() {
            Some(r) if (r.fg, r.bg, r.bold, r.underline) == (fg, bg, bold, underline) => r.text.push_str(text),
            _ => runs.push(Styled { text: text.to_string(), fg, bg, bold, underline }),
        };
        for line in range.start.line.0..=range.end.line.0 {
            let row = &grid[Line(line)];
            let (from, to) = match (range.is_block, line == range.start.line.0, line == range.end.line.0) {
                (true, ..) => (range.start.column.0, range.end.column.0),
                (false, true, true) => (range.start.column.0, range.end.column.0),
                (false, true, false) => (range.start.column.0, last),
                (false, false, true) => (0, range.end.column.0),
                (false, false, false) => (0, last),
            };
            // trailing blanks aren't copied, unless the line wraps on
            let wraps = !range.is_block && line != range.end.line.0 && row[Column(last)].flags.contains(Flags::WRAPLINE);
            let mut end = to.min(last);
            while !wraps && end > from && row[Column(end)].c == ' ' {
                end -= 1;
            }
            for col in from..=end {
                let cell = &row[Column(col)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                let bold = cell.flags.contains(Flags::BOLD);
                let mut fg = p.resolve(cell.fg, bold, if bold { p.bold_fg } else { p.fg }, colors);
                let mut bg = p.resolve(cell.bg, false, p.bg, colors);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let mut text = cell.c.to_string();
                text.extend(cell.zerowidth().unwrap_or(&[]));
                push(&text, fg, bg, bold, cell.flags.intersects(Flags::ALL_UNDERLINES));
            }
            if line != range.end.line.0 && !wraps {
                push("\n", p.fg, p.bg, false, false);
            }
        }
        runs
    }

    /// The whole scrollback and screen as text, keeping any selection.
    pub fn all_text(&mut self) -> String {
        let saved = self.term.selection.take();
        let top = self.term.grid().topmost_line();
        let bottom = self.term.grid().bottommost_line();
        let mut sel = Selection::new(SelectionType::Lines, Point::new(top, Column(0)), Side::Left);
        sel.update(Point::new(bottom, Column(0)), Side::Right);
        self.term.selection = Some(sel);
        let text = self.term.selection_to_string().unwrap_or_default();
        self.term.selection = saved;
        text.trim_end().to_string() + "\n"
    }

    /// The bytes to send for pasted text. Line ends become Enter, and in
    /// bracketed paste an escape is removed so that pasted text can't end the
    /// bracket and run as commands.
    pub fn paste_bytes(&mut self, text: &str) -> Vec<u8> {
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        // `PasteControls` off: as in PuTTY, after xterm, only tab, line ends,
        // Backspace and DEL get through
        let text: String = if self.paste_controls {
            text
        } else {
            text.chars().filter(|&c| !c.is_control() || matches!(c, '\t' | '\r' | '\x08' | '\x7f')).collect()
        };
        if self.bracketed_paste && self.term.mode().contains(TermMode::BRACKETED_PASTE) {
            let clean: String = text.chars().filter(|&c| c != '\x1b').collect();
            let mut out = b"\x1b[200~".to_vec();
            out.extend(self.encode_text(&clean));
            out.extend(b"\x1b[201~");
            out
        } else {
            self.encode_text(&text)
        }
    }

    /// Whether pasted line ends would run as commands, not go in as one paste.
    /// A single line end counts: one at the end of a line copied from a page
    /// runs it before it can be read.
    pub fn paste_runs_lines(&self, text: &str) -> bool {
        let bracketed = self.bracketed_paste && self.term.mode().contains(TermMode::BRACKETED_PASTE);
        !bracketed && text.contains(['\r', '\n'])
    }

    /// The session is over, so nothing more goes to a printer.
    pub fn stop_printing(&mut self) {
        self.filter.stop_printing();
    }

    pub fn app_keypad(&self) -> bool {
        self.term.mode().contains(TermMode::APP_KEYPAD)
    }

    /// Line feed/new line mode (`CSI 20 h`): Return sends CR LF.
    pub fn newline_mode(&self) -> bool {
        self.term.mode().contains(TermMode::LINE_FEED_NEW_LINE)
    }

    /// Ctrl+Pause: application keypad mode on or off, from this end.
    pub fn toggle_app_keypad(&mut self) {
        let seq: &[u8] = if self.app_keypad() { b"\x1b>" } else { b"\x1b=" };
        self.parser.advance(&mut self.term, seq);
    }

    pub fn screen(&self) -> Screen {
        let content = self.term.renderable_content();
        let (cols, lines) = self.size();
        let p = &self.palette;
        // DECSCNM turns every cell about, blank ones too
        let reverse = self.reverse.is_some();
        let (blank_fg, blank_bg) = if reverse { (p.bg, p.fg) } else { (p.fg, p.bg) };
        let blank = RenderCell { c: ' ', marks: ['\0'; MARKS], fg: blank_fg, bg: blank_bg, bold: false, underline: false, strike: false, blink: false, wide: false };
        let mut rows = vec![vec![blank; cols]; lines];
        let offset = content.display_offset as i32;
        let selection = content.selection;
        for item in content.display_iter {
            let cell = item.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let bold = cell.flags.contains(Flags::BOLD);
            let mut fg = p.resolve(cell.fg, bold, if bold { p.bold_fg } else { p.fg }, content.colors);
            let mut bg = p.resolve(cell.bg, false, p.bg, content.colors);
            // blinking text with BlinkText off has a bright background, as
            // PuTTY shows it
            let blink = cell.flags.contains(Flags::BLINK);
            if blink && !self.blink_text {
                bg = p.brighter_background(cell.bg, bg, content.colors);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = (fg.0 / 3 * 2, fg.1 / 3 * 2, fg.2 / 3 * 2);
            }
            let selected = selection.is_some_and(|s| s.contains(item.point));
            if cell.flags.contains(Flags::INVERSE) ^ selected ^ reverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            let row = item.point.line.0 + offset;
            if (0..lines as i32).contains(&row) {
                let mut marks = ['\0'; MARKS];
                for (slot, &m) in marks.iter_mut().zip(cell.zerowidth().unwrap_or(&[])) {
                    *slot = m;
                }
                rows[row as usize][item.point.column.0] = RenderCell {
                    c: cell.c,
                    marks,
                    fg,
                    bg,
                    bold,
                    underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                    strike: cell.flags.contains(Flags::STRIKEOUT),
                    blink: blink && self.blink_text,
                    wide: cell.flags.contains(Flags::WIDE_CHAR),
                };
            }
        }
        let c = content.cursor.point;
        let row = c.line.0 + offset;
        let visible = self.term.mode().contains(TermMode::SHOW_CURSOR) && (0..lines as i32).contains(&row);
        let mut cursor = visible.then_some((c.column.0, row as usize));
        // right-to-left text, and Arabic shaping, as PuTTY shows them
        if !self.no_bidi || !self.no_shaping {
            for (r, cells) in rows.iter_mut().enumerate() {
                let moved = self.lay_out_line(cells);
                if let (Some((col, cr)), Some(moved)) = (cursor, moved) {
                    if cr == r {
                        cursor = Some((moved.get(col).copied().unwrap_or(col), r));
                    }
                }
            }
        }
        Screen { rows, cursor }
    }

    /// Lays out one line with right-to-left text in it: runs of it turned
    /// about, brackets in them facing the other way, and Arabic letters in
    /// their joined forms. Says where each cell went, or nothing for a line
    /// with no right-to-left text, which stays as it is.
    fn lay_out_line(&self, cells: &mut Vec<RenderCell>) -> Option<Vec<usize>> {
        let chars: Vec<char> = cells.iter().map(|c| c.c).collect();
        if !crate::bidi::has_rtl(&chars) {
            return None;
        }
        // a wide character and the blank half after it move together
        let mut units: Vec<std::ops::Range<usize>> = Vec::new();
        let mut i = 0;
        while i < cells.len() {
            let len = if cells[i].wide && i + 1 < cells.len() { 2 } else { 1 };
            units.push(i..i + len);
            i += len;
        }
        let text: Vec<char> = units.iter().map(|u| cells[u.start].c).collect();
        let (order, rtl) = if self.no_bidi { ((0..units.len()).collect(), vec![false; units.len()]) } else { crate::bidi::visual_order(&text) };
        let mut laid = Vec::with_capacity(cells.len());
        let mut moved = vec![0; cells.len()];
        for &u in &order {
            for (k, from) in units[u].clone().enumerate() {
                moved[from] = laid.len();
                let mut cell = cells[from];
                if k == 0 && rtl[u] {
                    cell.c = crate::bidi::mirror(cell.c);
                }
                laid.push(cell);
            }
        }
        if !self.no_shaping {
            let starts: Vec<usize> = order.iter().map(|&u| moved[units[u].start]).collect();
            let mut shown: Vec<char> = starts.iter().map(|&p| laid[p].c).collect();
            crate::bidi::shape(&mut shown);
            for (&p, c) in starts.iter().zip(shown) {
                laid[p].c = c;
            }
        }
        *cells = laid;
        Some(moved)
    }

    /// One screen row as plain text, for tests.
    pub fn row_text(&self, row: usize) -> String {
        let s = self.screen();
        s.rows[row].iter().map(|c| c.c).collect::<String>().trim_end().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    #[test]
    fn text_reaches_the_screen() {
        let mut t = Terminal::new(40, 10);
        t.feed(b"hello\r\nworld");
        assert_eq!(t.row_text(0), "hello");
        assert_eq!(t.row_text(1), "world");
        assert_eq!(t.screen().cursor, Some((5, 1)));
    }

    #[test]
    fn colours_and_inverse() {
        let mut t = Terminal::new(10, 2);
        t.feed(b"\x1b[31mr\x1b[0;7mi\x1b[0;1;32mb");
        let row = &t.screen().rows[0];
        assert_eq!(row[0].fg, (187, 0, 0));
        assert_eq!((row[1].fg, row[1].bg), ((0, 0, 0), (187, 187, 187)));
        assert_eq!(row[2].fg, (85, 255, 85), "bold brightens");
    }

    #[test]
    fn selection_copies_and_clears() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"copy me please");
        t.select_start(5, 0);
        t.select_to(6, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me"));
        assert_ne!(t.screen().rows[0][5].bg, (0, 0, 0), "selection is highlighted");
        t.clear_selection();
        assert!(!t.has_selection());
    }

    #[test]
    fn new_settings_reach_a_running_terminal() {
        let mut t = Terminal::new(20, 3);
        t.feed(&b"line\r\n".repeat(10));
        let history = t.scroll_position().0;
        assert!(!t.scroll_on_key());
        let mut s = Settings::default();
        s.set("ScrollOnKey", Value::Int(1));
        s.set("Colour2", Value::Str("10,20,30".into()));
        t.reconfigure(&s);
        assert!(t.scroll_on_key(), "a terminal setting takes effect");
        assert_eq!(t.screen().rows[2][0].bg, (10, 20, 30), "so does a colour");
        assert_eq!(t.scroll_position().0, history, "the scrollback stays");
        assert_eq!(t.row_text(0).trim(), "line", "and so does the screen");
    }

    #[test]
    fn hosts_can_change_and_ask_about_the_palette() {
        let mut t = Terminal::new(10, 2);
        t.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b[31mx");
        assert_eq!(t.screen().rows[0][0].fg, (0x12, 0x34, 0x56), "OSC 4 changes red");
        t.feed(b"\x1b]P2abcdef");
        t.feed(b"\x1b[32my");
        assert_eq!(t.screen().rows[0][1].fg, (0xab, 0xcd, 0xef), "so does Linux's OSC P");
        t.take_events();
        t.feed(b"\x1b]4;1;?\x07");
        let replies: Vec<String> = t.take_events().into_iter().filter_map(|e| if let TermEvent::Reply(r) = e { Some(String::from_utf8_lossy(&r).into_owned()) } else { None }).collect();
        assert_eq!(replies.len(), 1, "{replies:?}");
        assert!(replies[0].starts_with("\x1b]4;1;rgb:1212/3434/5656"), "{replies:?}");
        t.feed(b"\x1b]R");
        assert_ne!(t.screen().rows[0][0].fg, (0x12, 0x34, 0x56), "OSC R puts the palette back");
    }

    #[test]
    fn reverse_video_turns_the_screen_about_and_a_quick_one_flashes() {
        let mut t = Terminal::new(10, 2);
        let normal = t.screen().rows[1][0];
        t.feed(b"\x1b[?5h");
        let turned = t.screen().rows[1][0];
        assert_eq!((turned.fg, turned.bg), (normal.bg, normal.fg));
        t.feed(b"\x1b[?5l");
        assert_eq!(t.screen().rows[1][0].bg, normal.bg);
        assert!(t.take_events().iter().any(|e| matches!(e, TermEvent::Flash)), "on and straight off shows as a flash");
        t.feed(b"\x1b[?5h");
        std::thread::sleep(FLASH_FOR + std::time::Duration::from_millis(20));
        t.feed(b"\x1b[?5l");
        assert!(!t.take_events().iter().any(|e| matches!(e, TermEvent::Flash)), "a long one was seen already");
    }

    #[test]
    fn older_alternate_screen_modes_work() {
        let mut t = Terminal::new(10, 3);
        t.feed(b"main\x1b[?1047hother");
        assert!(t.alt_screen());
        t.feed(b"\x1b[?1047l");
        assert!(!t.alt_screen());
        assert_eq!(t.row_text(0).trim(), "main", "the main screen is back as it was");
    }

    #[test]
    fn double_click_words_follow_puttys_classes() {
        let mut t = Terminal::new(40, 3);
        t.feed(b"cd /usr/local_bin user@host a=b");
        t.select_word(6, 0);
        assert_eq!(t.selected_text().as_deref(), Some("/usr/local_bin"), "- . / _ are word characters");
        t.select_word(19, 0);
        assert_eq!(t.selected_text().as_deref(), Some("user"), "@ is not");
        let mut s = Settings::default();
        // the line from 64 with @ (64) as part of a word
        s.set("Wordness64", Value::Str("2,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1".into()));
        t.reconfigure(&s);
        t.select_word(19, 0);
        assert_eq!(t.selected_text().as_deref(), Some("user@host"), "the setting changes it");
    }

    #[test]
    fn extend_moves_the_nearer_end() {
        let mut t = Terminal::new(40, 3);
        t.feed(b"0123456789abcdefghij");
        t.select_start(5, 0);
        t.select_to(14, 0);
        assert_eq!(t.selected_text().as_deref(), Some("56789abcde"));
        t.select_extend(2, 0);
        assert_eq!(t.selected_text().as_deref(), Some("23456789abcde"), "the start moved; the end stayed");
        t.select_extend(17, 0);
        assert_eq!(t.selected_text().as_deref(), Some("23456789abcdefgh"), "now the end moves");
    }

    #[test]
    fn paste_keeps_backspace_and_delete() {
        let mut t = Terminal::new(20, 3);
        assert_eq!(t.paste_bytes("a\x08b\x7fc\x01d"), b"a\x08b\x7fcd");
    }

    fn settings_of(pairs: &[(&str, Value)]) -> Settings {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, v.clone());
        }
        s
    }

    #[test]
    fn right_to_left_text_shows_right_to_left() {
        let mut t = Terminal::new(20, 2);
        t.feed("ab \u{5d0}\u{5d1}\u{5d2} (x)".as_bytes());
        assert_eq!(t.row_text(0), "ab \u{5d2}\u{5d1}\u{5d0} (x)");
        t.feed("\r\n\u{628}\u{628}\u{628}".as_bytes());
        assert_eq!(t.row_text(1), "\u{fe90}\u{fe92}\u{fe91}", "turned about and joined");
        assert_eq!(t.selected_text(), None);
        let mut plain = Terminal::with_settings(20, 2, &settings_of(&[("DisableBidi", Value::Int(1)), ("DisableArabicShaping", Value::Int(1))]));
        plain.feed("\u{5d0}\u{5d1} \u{628}\u{628}".as_bytes());
        assert_eq!(plain.row_text(0), "\u{5d0}\u{5d1} \u{628}\u{628}", "both can be turned off");
    }

    #[test]
    fn background_colour_erase_can_be_turned_off() {
        let mut on = Terminal::new(10, 3);
        on.feed(b"\x1b[44m\x1b[2J\r\n\n\n\n");
        let blue = on.screen().rows[0][5].bg;
        assert_ne!(blue, on.palette().bg, "BCE on: cleared cells take the colour");
        let mut off = Terminal::with_settings(10, 3, &settings_of(&[("BCE", Value::Int(0))]));
        let plain = off.screen().rows[0][5].bg;
        off.feed(b"\x1b[44m\x1b[2J\x1b[K\r\n\n\n\n");
        let screen = off.screen();
        assert!(screen.rows.iter().all(|r| r.iter().all(|c| c.bg == plain)), "BCE off: cleared and scrolled-in cells stay plain");
    }

    #[test]
    fn blinking_text_blinks_or_brightens() {
        let mut real = Terminal::with_settings(10, 2, &settings_of(&[("BlinkText", Value::Int(1))]));
        real.feed(b"\x1b[5mb\x1b[25mn");
        let row = &real.screen().rows[0];
        assert!(row[0].blink && !row[1].blink);
        let mut not = Terminal::new(10, 2);
        not.feed(b"\x1b[41mx\x1b[5my");
        let row = &not.screen().rows[0];
        assert!(!row[1].blink, "BlinkText off: no blinking");
        assert_ne!(row[0].bg, row[1].bg, "but a brighter background, as PuTTY");
    }

    #[test]
    fn ambiguous_width_characters_can_be_wide() {
        let mut narrow = Terminal::new(10, 2);
        narrow.feed("\u{b1}x".as_bytes());
        assert_eq!(narrow.row_text(0).trim_end(), "\u{b1}x");
        let mut wide = Terminal::with_settings(10, 2, &settings_of(&[("CJKAmbigWide", Value::Int(1))]));
        wide.feed("\u{b1}x".as_bytes());
        assert!(wide.screen().rows[0][0].wide, "± takes two cells");
    }

    #[test]
    fn line_drawing_in_utf8_needs_its_setting() {
        let mut plain = Terminal::new(10, 2);
        plain.feed(b"\x1b(0q\x1b(B");
        assert_eq!(plain.row_text(0).trim_end(), "q", "ignored in UTF-8, as PuTTY");
        let mut drawn = Terminal::with_settings(10, 2, &settings_of(&[("UTF8linedraw", Value::Int(1))]));
        drawn.feed(b"\x1b(0q\x1b(B");
        assert_eq!(drawn.row_text(0).trim_end(), "\u{2500}");
    }

    #[test]
    fn esc_percent_switches_to_utf8_and_back() {
        let mut t = Terminal::with_settings(20, 2, &settings_of(&[("LineCodePage", Value::Str("ISO-8859-1".into()))]));
        t.feed(b"\xe9");
        t.feed(b"\x1b%");
        t.feed(b"G\xc3\xa9\x1b%@\xe9");
        assert_eq!(t.row_text(0).trim_end(), "\u{e9}\u{e9}\u{e9}", "Latin-1, then UTF-8, then Latin-1 again, even split up");
    }

    #[test]
    fn rich_text_copy_keeps_colours_and_styles() {
        let mut t = Terminal::new(20, 3);
        t.feed(b"ab\x1b[1;31mcd\x1b[0m   \r\nef");
        t.select_start(0, 0);
        t.select_to(1, 1);
        let runs = t.selected_styled();
        let text: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "abcd\nef", "the same text as a plain copy, trailing blanks gone");
        let red = runs.iter().find(|r| r.text == "cd").expect("a run of its own");
        assert!(red.bold && red.fg != runs[0].fg);
        let rtf = rtf_styled(&runs, "Courier New", 10);
        assert!(rtf.starts_with("{\\rtf1") && rtf.contains("\\colortbl") && rtf.contains("\\b ") && rtf.contains("\\par"), "{rtf}");
        assert!(rtf.contains("cd}") && rtf.contains("ab}"), "{rtf}");
    }

    #[test]
    fn combining_marks_and_strikethrough_reach_the_screen() {
        let mut t = Terminal::new(10, 2);
        t.feed("e\u{301}x\x1b[9my".as_bytes());
        let row = &t.screen().rows[0];
        assert_eq!((row[0].c, row[0].marks[0]), ('e', '\u{301}'), "the accent stays with its letter");
        assert_eq!((row[1].c, row[1].marks[0]), ('x', '\0'), "and takes no cell of its own");
        assert!(row[2].strike && !row[1].strike);
    }

    #[test]
    fn clearing_without_scrollback_keeps_the_hosts_saved_cursor() {
        let mut s = Settings::default();
        s.set("EraseToScrollback", Value::Int(0));
        let mut t = Terminal::with_settings(20, 4, &s);
        t.feed(b"one\r\ntwo\r\nthree\x1b[2;3H\x1b7\x1b[4;1H\x1b[2J");
        assert_eq!(t.scroll_position().0, 0, "nothing went to the scrollback");
        assert!((0..4).all(|r| t.row_text(r).trim().is_empty()), "the screen is clear");
        t.feed(b"\x1b8X");
        assert_eq!(t.row_text(1).trim_end(), "  X", "the host's saved cursor survived");
    }

    #[test]
    fn a_refused_title_is_not_reported() {
        let mut s = Settings::default();
        s.set("NoRemoteWinTitle", Value::Int(1));
        s.set("RemoteQTitleAction", Value::Int(2));
        s.set("WinTitle", Value::Str("mine".into()));
        let mut t = Terminal::with_settings(20, 4, &s);
        t.feed(b"\x1b]2;theirs\x07");
        t.take_events();
        t.feed(b"\x1b[21t");
        let replies: Vec<_> = t.take_events().into_iter().filter_map(|e| if let TermEvent::Reply(r) = e { Some(r) } else { None }).collect();
        assert_eq!(replies, [b"\x1b]lmine\x1b\\".to_vec()]);
    }

    #[test]
    fn output_leaves_a_scrolled_back_view_where_it_is() {
        let mut t = Terminal::new(20, 5);
        for i in 0..100 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.scroll(30);
        let reading = t.row_text(0);
        for i in 100..400 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(t.row_text(0), reading, "the text being read stays on screen while output goes on");
        t.scroll_to_bottom();
        t.feed(b"latest");
        assert_eq!(t.row_text(4), "latest", "at the bottom, the view follows output");
        let mut s = Settings::default();
        s.set("ScrollOnDisp", Value::Int(1));
        let mut putty = Terminal::with_settings(20, 5, &s);
        for i in 0..100 {
            putty.feed(format!("line {i}\r\n").as_bytes());
        }
        putty.scroll(30);
        let reading = putty.row_text(0);
        putty.feed(b"more\r\n");
        assert_eq!(putty.row_text(0), reading, "even with PuTTY's ScrollOnDisp, as PuTTY's saved sessions have it");
    }

    #[test]
    fn the_menus_clear_and_reset_skip_the_host_filter() {
        let mut s = Settings::default();
        s.set("NoRemoteClearScroll", crate::settings::Value::Int(1));
        s.set("AutoWrapMode", crate::settings::Value::Int(0));
        let mut t = Terminal::with_settings(20, 3, &s);
        t.feed(&b"line\r\n".repeat(10));
        t.feed(b"\x1b[3J");
        assert!(t.scroll_position().0 > 0, "the host's request is refused");
        t.clear_scrollback();
        assert_eq!(t.scroll_position().0, 0, "the menu's isn't");
        t.feed(b"\x1b[?7h");
        t.reset();
        t.feed(&b"x".repeat(25));
        assert_eq!(t.row_text(1).trim(), "", "a reset keeps the session's own wrap setting");
    }

    #[test]
    fn output_over_a_selection_ends_it() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"copy me please\r\nsecond line");
        t.select_start(5, 0);
        t.select_to(6, 0);
        t.feed(b" more on line two");
        assert_eq!(t.selected_text().as_deref(), Some("me"), "output elsewhere leaves it");
        t.feed(b"\x1b[1;6Hus");
        assert!(!t.has_selection(), "output over the selected text ends it");
    }

    #[test]
    fn double_and_triple_click_select_words_and_lines() {
        let mut t = Terminal::new(30, 5);
        t.feed(b"alpha beta gamma\r\nnext");
        t.select_word(7, 0);
        assert_eq!(t.selected_text().as_deref(), Some("beta"));
        t.select_to(12, 0);
        assert_eq!(t.selected_text().as_deref(), Some("beta gamma"), "dragging goes by words");
        t.select_line(0);
        assert_eq!(t.selected_text().as_deref(), Some("alpha beta gamma\n"));
        t.select_to(2, 1);
        let text = t.selected_text().unwrap();
        assert!(text.starts_with("alpha beta gamma
") && text.contains("next"), "dragging goes by lines: {text:?}");
    }

    #[test]
    fn a_click_is_not_a_selection() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"text");
        t.select_start(1, 0);
        assert!(!t.has_selection(), "Ctrl+C after a bare click must still interrupt");
    }

    #[test]
    fn a_wobble_within_one_cell_is_still_a_click() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"text");
        t.select_start(1, 0);
        t.select_to(1, 0);
        assert!(!t.has_selection());
        t.select_to(2, 0);
        assert_eq!(t.selected_text().as_deref(), Some("ex"));
        t.select_to(1, 0);
        assert_eq!(t.selected_text().as_deref(), Some("e"), "after a real drag, one cell can be selected");
    }

    #[test]
    fn dragging_backwards_keeps_both_ends() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"copy me please");
        t.select_start(6, 0);
        t.select_to(5, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me"));
        t.feed(b"\r\nsecond");
        t.select_start(2, 1);
        t.select_to(5, 0);
        assert_eq!(t.selected_text().as_deref(), Some("me please\nsec"));
    }

    #[test]
    fn alt_screen_and_cursor_modes() {
        let mut t = Terminal::new(20, 5);
        assert!(!t.alt_screen());
        t.feed(b"\x1b[?1049h\x1b[?1h");
        assert!(t.alt_screen() && t.app_cursor());
        t.feed(b"\x1b[?1049l\x1b[?1l");
        assert!(!t.alt_screen() && !t.app_cursor());
    }

    #[test]
    fn plain_paste_turns_line_ends_into_enter() {
        let mut t = Terminal::new(20, 5);
        assert_eq!(t.paste_bytes("a\nb\r\nc"), b"a\rb\rc");
    }

    #[test]
    fn bracketed_paste_cant_escape_its_bracket() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b[?2004h");
        let got = t.paste_bytes("x\x1b[201~rm -rf /\n");
        assert_eq!(got, b"\x1b[200~x[201~rm -rf /\r\x1b[201~".to_vec());
    }

    #[test]
    fn any_line_end_that_would_run_asks_first() {
        let mut t = Terminal::new(20, 5);
        for runs in ["echo hi\n", "echo hi\r", "echo hi\r\n", "echo hi\n\n\n", "a\nb", "\n"] {
            assert!(t.paste_runs_lines(runs), "{runs:?}");
        }
        for stays in ["echo hi", "echo hi  \t", ""] {
            assert!(!t.paste_runs_lines(stays), "{stays:?}");
        }
        // in bracketed paste the line ends go in as text, so there's nothing to ask
        t.feed(b"\x1b[?2004h");
        assert!(!t.paste_runs_lines("echo hi\n"));
    }

    #[test]
    fn title_bell_and_replies_are_reported() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]0;my title\x07\x07\x1b[6n");
        let e = t.take_events();
        assert!(e.contains(&TermEvent::Title("my title".into())));
        assert!(e.contains(&TermEvent::Bell));
        assert!(e.contains(&TermEvent::Reply(b"\x1b[1;1R".to_vec())));
        assert!(t.take_events().is_empty());
    }

    #[test]
    fn host_cant_write_the_clipboard() {
        let mut t = Terminal::new(20, 5);
        t.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(t.take_events().is_empty());
    }

    #[test]
    fn copy_all_includes_scrollback() {
        let mut t = Terminal::new(20, 3);
        for i in 0..10 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        let all = t.all_text();
        assert!(all.starts_with("line0\n"), "{all:?}");
        assert!(all.contains("line9"));
    }

    #[test]
    fn resize_rewraps() {
        let mut t = Terminal::new(10, 3);
        t.feed(b"abcdefghijkl");
        assert_eq!(t.row_text(0), "abcdefghij");
        t.resize(20, 3);
        assert_eq!(t.size(), (20, 3));
        assert_eq!(t.row_text(0), "abcdefghijkl");
    }

    #[test]
    fn selection_follows_scrollback() {
        let mut t = Terminal::new(20, 3);
        for i in 0..10 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        t.scroll(5);
        let top = t.row_text(0);
        t.select_start(0, 0);
        t.select_to(4, 0);
        assert_eq!(t.selected_text().unwrap(), top);
        t.scroll_to_bottom();
    }

    #[test]
    fn colours_come_from_the_session() {
        let mut s = Settings::default();
        s.set("Colour2", crate::settings::Value::Str("10,20,30".into()));
        s.set("Colour8", crate::settings::Value::Str("1,2,3".into()));
        let mut t = Terminal::with_settings(10, 2, &s);
        t.feed(b"\x1b[31mx");
        let cell = t.screen().rows[0][0];
        assert_eq!((cell.fg, cell.bg), ((1, 2, 3), (10, 20, 30)));
    }

    #[test]
    fn mouse_is_only_reported_when_asked_for() {
        let mut t = Terminal::new(40, 10);
        const NO: (bool, bool) = (false, false);
        assert_eq!(t.mouse_report(0, 1, 2, true, false, NO), None);
        t.feed(b"\x1b[?1000h");
        assert_eq!(t.mouse_report(0, 1, 2, true, false, NO).unwrap(), vec![0x1b, b'[', b'M', 32, 34, 35]);
        assert_eq!(t.mouse_report(0, 1, 2, true, true, NO), None, "plain clicks mode has no motion");
        t.feed(b"\x1b[?1006h");
        assert_eq!(t.mouse_report(0, 1, 2, false, false, NO).unwrap(), b"\x1b[<0;2;3m");
    }

    #[test]
    fn mouse_reports_carry_modifiers_moves_and_urxvt_form() {
        let mut t = Terminal::new(300, 10);
        t.feed(b"\x1b[?1000h");
        assert_eq!(t.mouse_report(0, 1, 2, true, false, (true, true)).unwrap(), vec![0x1b, b'[', b'M', 32 + 20, 34, 35], "Shift is 4 and Ctrl 16");
        assert_eq!(t.mouse_report(0, 250, 2, true, false, (false, false)), None, "the old form can't reach column 251");
        assert_eq!(t.mouse_report(3, 1, 2, true, true, (false, false)), None, "a move with no button needs 1003");
        t.feed(b"\x1b[?1003h");
        assert_eq!(t.mouse_report(3, 1, 2, true, true, (false, false)).unwrap(), vec![0x1b, b'[', b'M', 32 + 35, 34, 35]);
        t.feed(b"\x1b[?1015h");
        assert_eq!(t.mouse_report(66, 250, 2, true, false, (false, false)).unwrap(), b"\x1b[98;251;3M", "urxvt's form, any column");
        t.feed(b"\x1b[?1015l");
        assert_eq!(t.mouse_report(0, 1, 2, true, false, (false, false)).unwrap(), vec![0x1b, b'[', b'M', 32, 34, 35]);
    }

    fn with(pairs: &[(&str, Value)]) -> Terminal {
        let mut s = Settings::default();
        for (k, v) in pairs {
            s.set(k, v.clone());
        }
        Terminal::with_settings(20, 4, &s)
    }

    #[test]
    fn wrap_can_start_off() {
        let mut t = with(&[("AutoWrapMode", Value::Int(0))]);
        t.feed(&[b'x'; 25]);
        assert_eq!(t.row_text(1), "");
        let mut t = with(&[]);
        t.feed(&[b'x'; 25]);
        assert_eq!(t.row_text(1), "xxxxx");
    }

    #[test]
    fn keypad_and_cursor_modes_can_start_on() {
        let t = with(&[("ApplicationKeypad", Value::Int(1)), ("ApplicationCursorKeys", Value::Int(1))]);
        assert!(t.app_keypad() && t.app_cursor());
        let t = with(&[]);
        assert!(!t.app_keypad() && !t.app_cursor());
    }

    #[test]
    fn a_legacy_character_set_both_ways() {
        let mut t = with(&[("LineCodePage", Value::Str("ISO-8859-1:1998 (Latin-1, West Europe)".into()))]);
        t.feed(&[0xe9]);
        assert_eq!(t.row_text(0), "é");
        assert_eq!(t.encode_text("é"), [0xe9]);
        let mut t = with(&[("LineCodePage", Value::Str("Win1251 (Cyrillic)".into()))]);
        t.feed(&[0xc0]);
        assert_eq!(t.row_text(0), "А");
        assert_eq!(with(&[("LineCodePage", Value::Str("UTF-8".into()))]).encode_text("é"), "é".as_bytes());
    }

    #[test]
    fn pasted_control_characters_are_dropped_by_default() {
        assert_eq!(with(&[]).paste_bytes("a\x1b[1mb\tc\n\x07"), b"a[1mb\tc\r");
        assert_eq!(with(&[("PasteControls", Value::Int(1))]).paste_bytes("a\x07"), b"a\x07");
    }

    #[test]
    fn rectangular_selection() {
        let mut t = with(&[("RectSelect", Value::Int(1))]);
        t.feed(b"abcd\r\nefgh");
        t.select_start_with(1, 0, false);
        t.select_to(2, 1);
        assert_eq!(t.selected_text().unwrap().trim(), "bc\nfg");
        let mut t = with(&[]);
        t.feed(b"abcd\r\nefgh");
        t.select_start_with(1, 0, true);
        t.select_to(2, 1);
        assert_eq!(t.selected_text().unwrap().trim(), "bc\nfg", "Alt flips the default");
    }

    #[test]
    fn cursor_shape_comes_from_the_session_and_the_host() {
        let mut t = with(&[("CurType", Value::Int(2)), ("BlinkCur", Value::Int(1))]);
        assert_eq!(t.cursor_style(), (CursorKind::Bar, true));
        t.feed(b"\x1b[4 q");
        assert_eq!(t.cursor_style(), (CursorKind::Underline, false));
    }

    #[test]
    fn the_real_title_is_reported_when_allowed() {
        let mut t = with(&[("RemoteQTitleAction", Value::Int(2))]);
        t.feed(b"\x1b]0;mine\x07");
        t.take_events();
        t.feed(b"\x1b[21t");
        assert!(t.take_events().contains(&TermEvent::Reply(b"\x1b]lmine\x1b\\".to_vec())));
    }

    #[test]
    fn rich_text_escapes_what_it_must() {
        let r = rtf("a{b}\\\né", "Courier New", 10);
        assert!(r.starts_with("{\\rtf1"));
        assert!(r.contains(r"\fs20 a\{b\}\\\par"), "{r}");
        assert!(r.ends_with("\\u233?}"), "{r}");
        assert!(rtf("x", "Evil}{\\", 10).contains("Evil;"));
    }

    #[test]
    fn wide_characters_are_marked() {
        let mut t = with(&[]);
        t.feed("日a".as_bytes());
        let s = t.screen();
        assert!(s.rows[0][0].wide && !s.rows[0][2].wide);
    }
}

#[cfg(test)]
mod scrollback {
    use super::*;

/// Everything a user sees scrolling from the top to the bottom, a line at
    /// a time: the top row at each step, then the last screen.
    fn scrolled_text(t: &mut Terminal) -> String {
        let (history, _) = t.scroll_position();
        let rows = t.size().1;
        let mut out = String::new();
        for up in (1..=history).rev() {
            t.scroll_to(up);
            out += &t.row_text(0);
            out.push('\n');
        }
        t.scroll_to(0);
        for r in 0..rows {
            out += &t.row_text(r);
            out.push('\n');
        }
        out
    }

    /// Which of lines `0..n` tagged `tag` can't be found by scrolling back,
    /// or in Copy All.
    fn missing(t: &mut Terminal, n: usize, tag: &str) -> Vec<usize> {
        let (seen, copied) = (scrolled_text(t), t.all_text());
        (0..n).filter(|i| !seen.contains(&format!("{tag}{i:04}x")) || !copied.contains(&format!("{tag}{i:04}x"))).collect()
    }

    #[test]
    fn scrolling_back_shows_what_copy_all_copies() {
        let mut t = term();
        t.feed(&lines("S", 9000, 10));
        let (seen, copied) = (scrolled_text(&mut t), t.all_text());
        let seen_missing = (0..9000).filter(|i| !seen.contains(&format!("S{i:04}x"))).count();
        let copied_missing = (0..9000).filter(|i| !copied.contains(&format!("S{i:04}x"))).count();
        assert_eq!((seen_missing, copied_missing), (0, 0));
    }

    fn lines(tag: &str, n: usize, width: usize) -> Vec<u8> {
        (0..n).flat_map(|i| format!("{tag}{i:04}x{}\r\n", "-".repeat(width)).into_bytes()).collect()
    }

    fn term() -> Terminal {
        Terminal::with_settings(80, 24, &Settings::default())
    }

    #[test]
    fn plain_output_is_all_kept() {
        let mut t = term();
        t.feed(&lines("A", 3000, 10));
        assert_eq!(missing(&mut t, 3000, "A"), Vec::<usize>::new());
    }

    #[test]
    fn apts_progress_line_loses_nothing_above_it() {
        // apt keeps "Progress: [ 40%]" on the last line by scrolling only the lines above
        let mut t = term();
        t.feed(b"\x1b7\x1b[0;23r\x1b8");
        for i in 0..500 {
            t.feed(format!("B{i:04}x Setting up\r\n").as_bytes());
            t.feed(b"\x1b7\x1b[24;0f\x1b[42mProgress: [ 40%]\x1b[49m\x1b8");
        }
        t.feed(b"\x1b7\x1b[0;24r\x1b8\x1b[24;0f\x1b[J");
        assert_eq!(missing(&mut t, 500, "B"), Vec::<usize>::new());
    }

    #[test]
    fn clearing_the_screen_keeps_it_in_scrollback() {
        let mut t = term();
        t.feed(&lines("C", 100, 10));
        t.feed(b"\x1b[H\x1b[2J");
        t.feed(&lines("D", 10, 10));
        assert_eq!(missing(&mut t, 100, "C"), Vec::<usize>::new());
    }

    #[test]
    fn a_putty_session_keeps_at_least_ten_thousand_lines() {
        let mut s = Settings::default();
        s.set("ScrollbackLines", crate::settings::Value::Int(2000));
        let mut t = Terminal::with_settings(80, 24, &s);
        t.feed(&lines("H", 9000, 10));
        assert_eq!(missing(&mut t, 9000, "H"), Vec::<usize>::new());
        // a session that asks for more gets more
        s.set("ScrollbackLines", crate::settings::Value::Int(20_000));
        assert_eq!(scrollback_lines(&s), 20_000);
    }

    #[test]
    fn long_lines_resizes_and_split_reads_lose_nothing() {
        let mut t = term();
        t.feed(&lines("E", 500, 200));
        assert_eq!(missing(&mut t, 500, "E"), Vec::<usize>::new());
        let mut t = term();
        t.feed(&lines("F", 500, 10));
        t.resize(60, 10);
        t.resize(120, 40);
        assert_eq!(missing(&mut t, 500, "F"), Vec::<usize>::new());
        let mut t = term();
        for c in lines("G", 1000, 30).chunks(7) {
            t.feed(c);
        }
        assert_eq!(missing(&mut t, 1000, "G"), Vec::<usize>::new());
    }
}

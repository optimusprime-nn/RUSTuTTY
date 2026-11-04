//! Right-to-left text and Arabic shaping, for showing a line, after PuTTY's
//! `terminal/bidi.c`: the line is laid out by the Unicode bidirectional
//! algorithm, then Arabic letters take the form their neighbours call for.
//! Only the display changes; the text, and what's copied, stay as sent.

use unicode_bidi::{BidiInfo, Level};

/// How an Arabic letter joins, as PuTTY's shaping types.
#[derive(Clone, Copy, PartialEq)]
enum Join {
    /// Joins on its left only; none in this block, kept for PuTTY's rules.
    #[allow(dead_code)]
    SL,
    /// Joins on its right: isolated and final forms.
    SR,
    /// Joins both ways: isolated, final, initial and medial forms.
    SD,
    /// Doesn't join.
    SU,
    /// Makes its neighbours join, as tatweel does.
    SC,
}
use Join::*;

/// The first character in `SHAPES`.
const SHAPE_FIRST: u32 = 0x621;

/// Each letter's joining and its isolated form, from U+0621 on, as PuTTY's
/// `shapetypes`. The final, initial and medial forms follow the isolated one.
const SHAPES: [(Join, u32); 178] = [
    (SU, 0xFE80), (SR, 0xFE81), (SR, 0xFE83), (SR, 0xFE85), (SR, 0xFE87), (SD, 0xFE89), (SR, 0xFE8D), (SD, 0xFE8F),
    (SR, 0xFE93), (SD, 0xFE95), (SD, 0xFE99), (SD, 0xFE9D), (SD, 0xFEA1), (SD, 0xFEA5), (SR, 0xFEA9), (SR, 0xFEAB),
    (SR, 0xFEAD), (SR, 0xFEAF), (SD, 0xFEB1), (SD, 0xFEB5), (SD, 0xFEB9), (SD, 0xFEBD), (SD, 0xFEC1), (SD, 0xFEC5),
    (SD, 0xFEC9), (SD, 0xFECD), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SC, 0x0000),
    (SD, 0xFED1), (SD, 0xFED5), (SD, 0xFED9), (SD, 0xFEDD), (SD, 0xFEE1), (SD, 0xFEE5), (SD, 0xFEE9), (SR, 0xFEED),
    (SR, 0xFEEF), (SD, 0xFEF1), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SR, 0xFB50), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SD, 0xFB66), (SD, 0xFB5E), (SD, 0xFB52), (SU, 0x0000), (SU, 0x0000), (SD, 0xFB56), (SD, 0xFB62), (SD, 0xFB5A),
    (SU, 0x0000), (SU, 0x0000), (SD, 0xFB76), (SD, 0xFB72), (SU, 0x0000), (SD, 0xFB7A), (SD, 0xFB7E), (SR, 0xFB88),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SR, 0xFB84), (SR, 0xFB82), (SR, 0xFB86), (SU, 0x0000), (SU, 0x0000),
    (SR, 0xFB8C), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SR, 0xFB8A),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SD, 0xFB6A), (SU, 0x0000), (SD, 0xFB6E), (SU, 0x0000), (SU, 0x0000),
    (SD, 0xFB8E), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SD, 0xFBD3), (SU, 0x0000), (SD, 0xFB92), (SU, 0x0000),
    (SD, 0xFB9A), (SU, 0x0000), (SD, 0xFB96), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SR, 0xFB9E), (SD, 0xFBA0), (SU, 0x0000), (SU, 0x0000), (SD, 0xFBAA), (SU, 0x0000), (SR, 0xFBA4),
    (SD, 0xFBA6), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SR, 0xFBE0), (SR, 0xFBD9), (SR, 0xFBD7), (SR, 0xFBDB),
    (SR, 0xFBE2), (SU, 0x0000), (SR, 0xFBDE), (SD, 0xFBFC), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000), (SU, 0x0000),
    (SU, 0x0000), (SR, 0xFBAE),
];

fn join(c: char) -> Join {
    let i = (c as u32).wrapping_sub(SHAPE_FIRST) as usize;
    SHAPES.get(i).map_or(SU, |s| s.0)
}

fn isolated(c: char) -> u32 {
    SHAPES[(c as u32 - SHAPE_FIRST) as usize].1
}

fn form(code: u32) -> char {
    char::from_u32(code).unwrap_or(' ')
}

/// Whether a line has anything written right to left, so lines without it
/// can skip the work.
pub fn has_rtl(text: &[char]) -> bool {
    text.iter().any(|&c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF))
}

/// The order to show a line's characters in, left to right: indexes into
/// `text`. The line itself runs left to right, as PuTTY's do, and runs of
/// right-to-left text inside it are turned about. Also says which characters
/// ended up right to left, whose brackets face the other way.
pub fn visual_order(text: &[char]) -> (Vec<usize>, Vec<bool>) {
    let s: String = text.iter().collect();
    // byte offset of each character, to map the algorithm's ranges back
    let starts: Vec<usize> = s.char_indices().map(|(b, _)| b).collect();
    let at = |byte: usize| starts.binary_search(&byte).unwrap_or_else(|i| i);
    let info = BidiInfo::new(&s, Some(Level::ltr()));
    let mut order = Vec::with_capacity(text.len());
    let mut rtl = vec![false; text.len()];
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let (from, to) = (at(run.start), at(run.end));
            let right_to_left = levels[run.start].is_rtl();
            let indexes: Vec<usize> = (from..to).collect();
            if right_to_left {
                for &i in indexes.iter().rev() {
                    rtl[i] = true;
                    order.push(i);
                }
            } else {
                order.extend(indexes);
            }
        }
    }
    (order, rtl)
}

/// A bracket as it shows in right-to-left text.
pub fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '\u{ab}' => '\u{bb}',
        '\u{bb}' => '\u{ab}',
        c => c,
    }
}

/// Arabic letters in the forms their neighbours call for, as PuTTY's
/// `do_shape`, on a line already in the order it shows. Lam and alef become
/// one ligature, which takes the lam's place and leaves a space in the alef's.
pub fn shape(line: &mut [char]) {
    let orig = line.to_vec();
    let joins = |t: Join| matches!(t, SL | SD | SC);
    let joined_from = |t: Join| matches!(t, SR | SD | SC);
    for i in 0..orig.len() {
        let c = orig[i];
        let next = if i + 1 < orig.len() { join(orig[i + 1]) } else { SU };
        let prev = if i > 0 { join(orig[i - 1]) } else { SU };
        match join(c) {
            SR => line[i] = form(isolated(c) + if joins(next) { 1 } else { 0 }),
            SD => {
                if c == '\u{644}' && i > 0 {
                    let lig = match orig[i - 1] {
                        '\u{622}' => Some(0xFEF5),
                        '\u{623}' => Some(0xFEF7),
                        '\u{625}' => Some(0xFEF9),
                        '\u{627}' => Some(0xFEFB),
                        _ => None,
                    };
                    if let Some(l) = lig {
                        line[i] = form(l + if joins(next) { 1 } else { 0 });
                        line[i - 1] = ' ';
                        continue;
                    }
                }
                line[i] = match (joins(next), joined_from(prev)) {
                    (true, true) => form(isolated(c) + 3),
                    (true, false) => form(isolated(c) + 1),
                    (false, true) => form(isolated(c) + 2),
                    (false, false) => form(isolated(c)),
                };
            }
            SL | SU | SC => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hebrew_runs_right_to_left_inside_a_left_to_right_line() {
        let text: Vec<char> = "ab \u{5d0}\u{5d1}\u{5d2} cd".chars().collect();
        let (order, rtl) = visual_order(&text);
        let shown: String = order.iter().map(|&i| text[i]).collect();
        assert_eq!(shown, "ab \u{5d2}\u{5d1}\u{5d0} cd");
        assert!(rtl[3] && !rtl[0]);
    }

    #[test]
    fn arabic_letters_take_their_joined_forms() {
        // beh beh beh, shown right to left: the visual order is reversed
        let mut line: Vec<char> = "\u{628}\u{628}\u{628}".chars().collect();
        shape(&mut line);
        // shown right to left, the word starts on the right: initial form
        // there, medial in the middle, final on the left
        assert_eq!(line, ['\u{fe90}', '\u{fe92}', '\u{fe91}']);
        let mut alone = vec!['\u{628}'];
        shape(&mut alone);
        assert_eq!(alone, ['\u{fe8f}'], "on its own, the isolated form");
    }

    #[test]
    fn lam_alef_becomes_one_ligature() {
        let mut line = vec!['\u{627}', '\u{644}'];
        shape(&mut line);
        assert_eq!(line, [' ', '\u{fefb}']);
    }
}

// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Receipt text: cleaning, composing, wrapping and the Windows-1252 table. Pure, no IO.
//!
//! The CP1252 table is unicode.org's `CP1252.TXT`:
//! <https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/WINDOWS/CP1252.TXT>

/// Make one string safe and printable: line ends become `\n`, tab, NBSP and the narrow no-break
/// space U+202F become a space, control characters (C0, U+007F, C1; `\n` stays) and invisible
/// ones (zero-width, direction marks U+202A-202E and U+2066-2069, U+2060-2064) are dropped, decomposed Latin-1
/// letters are composed, other combining marks are dropped, and U+1E9E becomes `SS` (the font has
/// no glyph for it). A character the font has no glyph for is kept: it prints as a box.
pub fn normalize(s: &str) -> String {
    let mut kept: Vec<char> = Vec::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                kept.push('\n');
            }
            '\n' => kept.push('\n'),
            '\t' | '\u{a0}' => kept.push(' '),
            c if c.is_control() => {}
            '\u{202f}' => kept.push(' '),
            '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ad}' => {}
            '\u{1e9e}' => kept.extend(['S', 'S']),
            c => kept.push(c),
        }
    }
    let mut out = String::with_capacity(kept.len());
    let mut last: Option<char> = None;
    for c in kept {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            if let Some(b) = last
                && let Some(composed) = compose(b, c)
            {
                last = Some(composed);
            }
            continue;
        }
        if let Some(b) = last.replace(c) {
            out.push(b);
        }
    }
    out.extend(last);
    out
}

/// Base letter plus combining mark -> the precomposed Latin-1 letter (and Ÿ).
fn compose(base: char, mark: char) -> Option<char> {
    let (upper, lower): (&str, &str) = match mark {
        '\u{300}' => ("AEIOU", "ÀÈÌÒÙ"),
        '\u{301}' => ("AEIOUY", "ÁÉÍÓÚÝ"),
        '\u{302}' => ("AEIOU", "ÂÊÎÔÛ"),
        '\u{303}' => ("ANO", "ÃÑÕ"),
        '\u{308}' => ("AEIOUY", "ÄËÏÖÜŸ"),
        '\u{30a}' => ("A", "Å"),
        '\u{327}' => ("C", "Ç"),
        _ => return None,
    };
    let is_lower = base.is_ascii_lowercase();
    let b = base.to_ascii_uppercase();
    let at = upper.chars().position(|u| u == b)?;
    let composed = lower.chars().nth(at)?;
    if !is_lower {
        return Some(composed);
    }
    // ÿ is U+00FF, the one lower-case form that is not "upper + 0x20"
    if composed == 'Ÿ' {
        return Some('ÿ');
    }
    char::from_u32(composed as u32 + 0x20)
}

/// Break `text` (already normalised) into rows of at most `width` characters. A paragraph that
/// fits is kept as it is, inner and leading spaces included; otherwise the row ends at the last
/// space that fits (the spaces there are dropped), or after `width` characters when there is none.
/// Left to right only. Empty paragraphs are empty rows, and an empty text is one empty row.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for para in text.split('\n') {
        let mut rest: Vec<char> = para.chars().collect();
        loop {
            if rest.len() <= width {
                rows.push(rest.iter().collect());
                break;
            }
            // a space at index `width` ends the row exactly at the edge
            let brk = (1..=width).rev().find(|&i| rest[i] == ' ');
            match brk {
                Some(i) if rest[..i].iter().any(|&c| c != ' ') => {
                    let end = rest[..i]
                        .iter()
                        .rposition(|&c| c != ' ')
                        .map_or(0, |p| p + 1);
                    rows.push(rest[..end].iter().collect());
                    let skip = rest[i..].iter().take_while(|&&c| c == ' ').count();
                    rest.drain(..i + skip);
                }
                _ => {
                    rows.push(rest[..width].iter().collect());
                    rest.drain(..width);
                }
            }
            if rest.is_empty() {
                break;
            }
        }
    }
    rows
}

/// The CP1252 byte of `c`, or `None` when it has none (U+007F, the C1 controls, the five holes
/// 0x81 0x8D 0x8F 0x90 0x9D and everything outside the table).
pub fn cp1252(c: char) -> Option<u8> {
    let u = c as u32;
    match u {
        0x20..=0x7e | 0xa0..=0xff => Some(u as u8),
        _ => CP1252_HIGH
            .iter()
            .find(|&&(_, ch)| ch == c)
            .map(|&(b, _)| b),
    }
}

/// 0x80-0x9F of CP1252 (27 entries; 0x81, 0x8D, 0x8F, 0x90 and 0x9D are undefined).
pub const CP1252_HIGH: [(u8, char); 27] = [
    (0x80, '\u{20ac}'),
    (0x82, '\u{201a}'),
    (0x83, '\u{0192}'),
    (0x84, '\u{201e}'),
    (0x85, '\u{2026}'),
    (0x86, '\u{2020}'),
    (0x87, '\u{2021}'),
    (0x88, '\u{02c6}'),
    (0x89, '\u{2030}'),
    (0x8a, '\u{0160}'),
    (0x8b, '\u{2039}'),
    (0x8c, '\u{0152}'),
    (0x8e, '\u{017d}'),
    (0x91, '\u{2018}'),
    (0x92, '\u{2019}'),
    (0x93, '\u{201c}'),
    (0x94, '\u{201d}'),
    (0x95, '\u{2022}'),
    (0x96, '\u{2013}'),
    (0x97, '\u{2014}'),
    (0x98, '\u{02dc}'),
    (0x99, '\u{2122}'),
    (0x9a, '\u{0161}'),
    (0x9b, '\u{203a}'),
    (0x9c, '\u{0153}'),
    (0x9e, '\u{017e}'),
    (0x9f, '\u{0178}'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ends_and_blanks() {
        assert_eq!(normalize("a\r\nb\rc\nd"), "a\nb\nc\nd");
        assert_eq!(normalize("a\tb\u{a0}c"), "a b c");
        assert_eq!(normalize("a\u{0}b\u{1b}c\u{7f}d\u{85}e\u{9f}f"), "abcdef");
        assert_eq!(
            normalize("a\u{200b}b\u{200f}c\u{feff}d\u{ad}e\u{fe0f}f"),
            "abcdef"
        );
        // direction marks and isolates (copied phone numbers carry them), word joiner and the
        // invisible operators have no glyph: they would print as boxes
        assert_eq!(
            normalize(
                "Tel: \u{202d}+49 151 234\u{202c} a\u{202a}b\u{202e}c\u{2060}d\u{2064}e\u{2066}f\u{2069}g"
            ),
            "Tel: +49 151 234 abcdefg"
        );
        // the narrow no-break space of French thousands is a space
        assert_eq!(normalize("1\u{202f}234,56 €"), "1 234,56 €");
    }

    #[test]
    fn decomposed_letters_compose() {
        assert_eq!(normalize("u\u{308}"), "ü");
        assert_eq!(normalize("A\u{308}O\u{308}U\u{308}"), "ÄÖÜ");
        assert_eq!(
            normalize("e\u{301}a\u{300}o\u{302}n\u{303}a\u{30a}c\u{327}"),
            "éàôñåç"
        );
        assert_eq!(normalize("y\u{308}Y\u{308}"), "ÿŸ");
        // no Latin-1 form: the mark is dropped
        assert_eq!(normalize("x\u{301}z\u{308}"), "xz");
        assert_eq!(normalize("\u{301}a"), "a");
        // an already composed text is left alone
        assert_eq!(normalize("Café Müller ß €"), "Café Müller ß €");
    }

    #[test]
    fn sharp_s_capital_becomes_two_letters() {
        assert_eq!(normalize("STRA\u{1e9e}E"), "STRASSE");
    }

    #[test]
    fn emoji_is_kept_for_the_font_to_box() {
        assert_eq!(normalize("a😀b"), "a😀b");
    }

    #[test]
    fn nothing_that_can_be_a_command_survives() {
        for s in ["\u{1b}@", "\u{1d}V\u{0}", "a\u{0}b", "\u{1b}d\u{9}"] {
            let n = normalize(s);
            assert!(n.chars().all(|c| !c.is_control() || c == '\n'), "{n:?}");
        }
    }

    #[test]
    fn cp1252_table() {
        for b in 0x20u8..=0x7e {
            assert_eq!(cp1252(b as char), Some(b));
        }
        for b in 0xa0u8..=0xff {
            assert_eq!(cp1252(char::from(b)), Some(b));
        }
        let holes = [0x81u8, 0x8d, 0x8f, 0x90, 0x9d];
        let mut seen = 0;
        for b in 0x80u8..=0x9f {
            let found: Vec<char> = (0u32..0x2200)
                .filter_map(char::from_u32)
                .filter(|&c| cp1252(c) == Some(b))
                .collect();
            if holes.contains(&b) {
                assert!(found.is_empty(), "{b:#x}");
            } else {
                assert_eq!(found.len(), 1, "{b:#x}");
                seen += 1;
            }
        }
        assert_eq!(seen, 27);
        assert_eq!(cp1252('€'), Some(0x80));
        assert_eq!(cp1252('ß'), Some(0xdf));
        assert_eq!(cp1252('Ÿ'), Some(0x9f));
        // refused: DEL, the C1 controls, controls, and what the table does not have
        for c in [
            '\u{7f}', '\u{80}', '\u{81}', '\u{9f}', '\u{0}', '\u{1f}', '\n', 'Ж', '😀', '\u{1e9e}',
        ] {
            assert_eq!(cp1252(c), None, "{c:?}");
        }
    }

    #[test]
    fn wrap_keeps_what_fits() {
        assert_eq!(wrap("  a  b  ", 32), ["  a  b  "]);
        assert_eq!(wrap("", 32), [""]);
        assert_eq!(wrap("abcd", 4), ["abcd"]);
        assert_eq!(wrap("a\n\nb", 4), ["a", "", "b"]);
    }

    #[test]
    fn wrap_breaks_at_the_last_space_that_fits() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("aaa bbb ccc", 8), ["aaa bbb", "ccc"]);
        // a space exactly after the edge still ends the row there
        assert_eq!(wrap("abcd efgh", 4), ["abcd", "efgh"]);
        assert_eq!(wrap("aaa    bbb", 4), ["aaa", "bbb"]);
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("ab cdefghij", 4), ["ab", "cdef", "ghij"]);
        assert_eq!(wrap("          x", 4), ["    ", "    ", "  x"]);
    }
}

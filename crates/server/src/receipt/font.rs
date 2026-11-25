// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The 12 x 24 dot font of the drawn text: Terminus Font 4.49.1 (SIL OFL 1.1), `ter-u24n.bdf` and
//! `ter-u24b.bdf`, embedded unmodified from `crates/server/assets/terminus/`. Every glyph has the
//! box `BBX 12 24 0 -5`, so a character cell is 12 dots wide and 24 high.
//!
//! Source of the files: terminus-font-4.49.1.tar.gz,
//! <https://downloads.sourceforge.net/project/terminus-font/terminus-font-4.49/terminus-font-4.49.1.tar.gz>.
//! The BDF format (`ENCODING n`, 24 hex lines after `BITMAP`, high bit = leftmost dot) is Adobe's
//! BDF 2.2 specification.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const CELL_W: u32 = 12;
pub const CELL_H: u32 = 24;
/// The glyph that stands for a character the font does not have.
const REPLACEMENT: char = '\u{fffd}';

const NORMAL: &str = include_str!("../../assets/terminus/ter-u24n.bdf");
const BOLD: &str = include_str!("../../assets/terminus/ter-u24b.bdf");

/// One weight of the font: 24 rows per glyph, the 12 dots in the top bits of each `u16`.
pub struct Font {
    glyphs: HashMap<char, [u16; 24]>,
}

impl Font {
    fn parse(bdf: &str) -> Font {
        let mut glyphs = HashMap::new();
        let mut lines = bdf.lines();
        let mut code: Option<i64> = None;
        while let Some(line) = lines.next() {
            if let Some(n) = line.strip_prefix("ENCODING ") {
                code = n.trim().parse().ok();
            } else if line == "BITMAP" {
                let mut rows = [0u16; 24];
                for row in rows.iter_mut() {
                    *row = lines
                        .next()
                        .and_then(|l| u16::from_str_radix(l.trim(), 16).ok())
                        .unwrap_or(0);
                }
                // encoding 0 is the font's own "missing" glyph, not a character
                if let Some(c) = code
                    .filter(|&c| c > 0)
                    .and_then(|c| u32::try_from(c).ok())
                    .and_then(char::from_u32)
                {
                    glyphs.insert(c, rows);
                }
            }
        }
        Font { glyphs }
    }

    /// The glyph of `c`, or the replacement box.
    pub fn glyph(&self, c: char) -> &[u16; 24] {
        self.glyphs
            .get(&c)
            .or_else(|| self.glyphs.get(&REPLACEMENT))
            .expect("the font has U+FFFD")
    }

    pub fn has(&self, c: char) -> bool {
        self.glyphs.contains_key(&c)
    }

    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

/// The normal or the bold weight, parsed once.
pub fn font(bold: bool) -> &'static Font {
    static N: OnceLock<Font> = OnceLock::new();
    static B: OnceLock<Font> = OnceLock::new();
    if bold {
        B.get_or_init(|| Font::parse(BOLD))
    } else {
        N.get_or_init(|| Font::parse(NORMAL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn the_assets_are_the_unmodified_terminus_files() {
        assert_eq!(
            hex(&Sha256::digest(NORMAL.as_bytes())),
            "ff640e9e097983355e8f70ed8fb645bd850184a0d590de17f6fc9ceec1bf8eaf"
        );
        assert_eq!(
            hex(&Sha256::digest(BOLD.as_bytes())),
            "759e0dbbd137d0132cee5d83b681cea1c8a656f161e04f83e121525ea4eb1824"
        );
    }

    #[test]
    fn both_weights_parse_to_the_whole_set() {
        for bold in [false, true] {
            let f = font(bold);
            assert!(f.len() >= 1355, "{} glyphs", f.len());
            for c in ['A', 'ü', 'ß', '€', '\u{fffd}', ' ', 'Ж', 'ä'] {
                assert!(f.has(c), "{c:?} bold={bold}");
            }
            // a space is empty, a letter is not
            assert!(f.glyph(' ').iter().all(|&r| r == 0));
            assert!(f.glyph('A').iter().any(|&r| r != 0));
            // only the 12 high bits are ever set
            assert!(f.glyph('W').iter().all(|&r| r & 0x000f == 0));
        }
    }

    #[test]
    fn bold_differs_from_normal_and_unknown_is_a_box() {
        assert_ne!(font(true).glyph('A'), font(false).glyph('A'));
        let f = font(false);
        assert!(!f.has('😀'));
        assert_eq!(f.glyph('😀'), f.glyph('\u{fffd}'));
        assert!(f.glyph('\u{fffd}').iter().any(|&r| r != 0));
    }
}

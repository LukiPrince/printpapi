// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The ESC/POS byte stream of a layout. See the module header of `receipt` for the commands and
//! their sources. A drawn row, an image, a QR code and a barcode all leave as `GS v 0` bitmaps;
//! consecutive ones are drawn into one bitmap and sent in bands. A native text row goes out in the
//! printer's own font where every character has a Windows-1252 byte.

use super::layout::{Layout, Op};
use super::raster::draw_ops;
use super::text::cp1252;
use super::{Codepage, CutMode, Profile, pitch};

/// Rows of one `GS v 0` band.
const BAND_ROWS: usize = 128;

/// A text row (`h` times the cell height) as the printer's own font would print it, or `None`
/// when the row has to be drawn: the profile says raster, some character has no Windows-1252
/// byte, or it is a blank line taller than one feed.
fn native(runs: &[(String, bool)], h: u8, p: &Profile) -> Option<Vec<(bool, Vec<u8>)>> {
    if p.codepage != Codepage::Cp1252 {
        return None;
    }
    let mut out: Vec<(bool, Vec<u8>)> = Vec::new();
    for (text, bold) in runs {
        let bytes: Option<Vec<u8>> = text.chars().map(cp1252).collect();
        out.push((*bold, bytes?));
    }
    // trailing blanks cost bytes and print nothing
    while let Some((_, last)) = out.last_mut() {
        while last.last() == Some(&b' ') {
            last.pop();
        }
        if last.is_empty() {
            out.pop();
        } else {
            break;
        }
    }
    // A blank line is a bare LF, which feeds one line at the line spacing (30 dots) whatever the
    // character size. The layout counts `pitch(h)`, so a taller blank line is a white bitmap.
    if out.is_empty() && pitch(h) > 30 {
        return None;
    }
    Some(out)
}

struct Stream<'a> {
    out: Vec<u8>,
    p: &'a Profile,
    size: (u8, u8),
    bold: bool,
    pending: Vec<&'a Op>,
}

impl<'a> Stream<'a> {
    /// Send the drawn ops collected so far as one bitmap in bands. Right of the last black byte
    /// of a band is cut off (at least one byte); a white band is sent too, it carries the feed.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let bm = draw_ops(self.p.width_dots, &self.pending);
        self.pending.clear();
        let bpr = bm.bytes_per_row;
        let mut y = 0;
        while y < bm.h as usize {
            let rows = BAND_ROWS.min(bm.h as usize - y);
            let band = &bm.data[y * bpr..(y + rows) * bpr];
            let used = (0..bpr)
                .rev()
                .find(|&x| (0..rows).any(|r| band[r * bpr + x] != 0))
                .map_or(1, |x| x + 1);
            self.out.extend_from_slice(&[0x1d, 0x76, 0x30, 0x00]);
            self.out.extend_from_slice(&(used as u16).to_le_bytes());
            self.out.extend_from_slice(&(rows as u16).to_le_bytes());
            for r in 0..rows {
                self.out.extend_from_slice(&band[r * bpr..r * bpr + used]);
            }
            y += rows;
        }
    }

    fn native_row(&mut self, parts: &[(bool, Vec<u8>)], size: (u8, u8)) {
        if size != self.size {
            self.out
                .extend_from_slice(&[0x1d, 0x21, ((size.0 - 1) << 4) | (size.1 - 1)]);
            self.size = size;
        }
        for (bold, bytes) in parts {
            if *bold != self.bold {
                self.out.extend_from_slice(&[0x1b, 0x45, u8::from(*bold)]);
                self.bold = *bold;
            }
            self.out.extend_from_slice(bytes);
        }
        self.out.push(0x0a);
    }
}

/// The bytes for one receipt. The stream starts and ends with `ESC @`, so no code page, size or
/// bold stays behind for the next raw job of another client.
pub fn escpos(l: &Layout, p: &Profile) -> Vec<u8> {
    let mut s = Stream {
        out: vec![0x1b, 0x40],
        p,
        size: (1, 1),
        bold: false,
        pending: Vec::new(),
    };
    if p.codepage == Codepage::Cp1252 {
        // ESC R 0: the international set is USA, so [ \ ] { | } ~ # $ @ stay ASCII
        s.out
            .extend_from_slice(&[0x1b, 0x52, 0x00, 0x1b, 0x74, p.codepage_number]);
    }
    for op in &l.ops {
        match op {
            Op::Row { runs, w, h } => match native(runs, *h, p) {
                Some(parts) => {
                    s.flush();
                    s.native_row(&parts, (*w, *h));
                }
                None => s.pending.push(op),
            },
            Op::Image { .. } => s.pending.push(op),
            Op::Feed(n) => {
                s.flush();
                s.out.extend_from_slice(&[0x1b, 0x64, *n]);
            }
            Op::Cut => {
                s.flush();
                match p.cut {
                    CutMode::Partial => s.out.extend_from_slice(&[0x1d, 0x56, 0x42, 0x00]),
                    CutMode::Full => s.out.extend_from_slice(&[0x1d, 0x56, 0x41, 0x00]),
                    CutMode::None => s.out.extend_from_slice(&[0x1b, 0x64, 0x04]),
                }
            }
            Op::Drawer(pin) => {
                s.flush();
                s.out
                    .extend_from_slice(&[0x1b, 0x70, u8::from(*pin == 5), 0x19, 0xfa]);
            }
        }
    }
    s.flush();
    s.out.extend_from_slice(&[0x1b, 0x40]);
    s.out
}

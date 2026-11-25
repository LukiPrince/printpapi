// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The one layout pass: elements become rows of text, bitmaps, feeds, cuts and drawer kicks on a
//! fixed grid of 12 x 24 dot cells. ESC/POS, the PDF and the preview all read this result.
//! Alignment is padding (spaces in a row, a dot offset for an image); `ESC a` is never used.

use super::codes;
use super::text::{normalize, wrap};
use super::{
    Align, BarFormat, CELL_W, Cell, CutMode, Doc, Element, MAX_IMAGE_ALLOC, MAX_IMAGE_PIXELS,
    MAX_ROWS, Profile, pitch,
};
use crate::label::{Bitmap, decode, resize_gray};
use image::imageops;

/// What a feed line is worth in dots (the power-on line spacing).
const FEED_DOTS: u32 = 30;

#[derive(Debug, Clone)]
pub enum Op {
    /// A row of text: (text, bold) runs, `w` x `h` times the cell size. The text holds its own
    /// alignment padding.
    Row {
        runs: Vec<(String, bool)>,
        w: u8,
        h: u8,
    },
    /// A bitmap at a dot offset from the left edge.
    Image {
        x: u32,
        bm: Bitmap,
    },
    Feed(u8),
    Cut,
    Drawer(u8),
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub width_dots: u32,
    pub ops: Vec<Op>,
}

impl Layout {
    /// Whether anything is printed on paper (a row or an image), as opposed to feeds, cuts and
    /// drawer kicks alone.
    pub fn has_pages(&self) -> bool {
        self.ops
            .iter()
            .any(|o| matches!(o, Op::Row { .. } | Op::Image { .. }))
    }
}

/// Without a cutter a cut is `ESC d 4`: four lines of paper.
fn cut_feed(p: &Profile) -> u32 {
    if p.cut == CutMode::None {
        4 * FEED_DOTS
    } else {
        0
    }
}

fn too_long() -> String {
    format!("receipt is longer than {MAX_ROWS} dots")
}

fn pad(line: &str, width: usize, align: Align, fill_right: bool) -> String {
    let len = line.chars().count();
    let room = width.saturating_sub(len);
    let (left, right) = match align {
        Align::Left => (0, room),
        Align::Center => (room / 2, room - room / 2),
        Align::Right => (room, 0),
    };
    let mut s = " ".repeat(left);
    s.push_str(line);
    if fill_right {
        s.push_str(&" ".repeat(right));
    }
    s
}

fn x_of(align: Align, width: u32, w: u32) -> u32 {
    match align {
        Align::Left => 0,
        Align::Center => width.saturating_sub(w) / 2,
        Align::Right => width.saturating_sub(w),
    }
}

/// Cell widths of a columns row: the fixed ones, the rest shared equally, the remainder to the
/// first free cell. One blank character sits between cells.
fn cell_widths(cells: &[Cell], total: usize) -> Option<Vec<usize>> {
    let gaps = cells.len() - 1;
    let fixed: usize = cells
        .iter()
        .filter_map(|c| c.width)
        .map(|w| w as usize)
        .sum();
    let free = cells.iter().filter(|c| c.width.is_none()).count();
    let used = fixed + gaps;
    if used > total {
        return None;
    }
    let rest = total - used;
    if free > 0 && rest < free {
        return None;
    }
    let mut out = Vec::with_capacity(cells.len());
    let mut first_free = true;
    for c in cells {
        out.push(match c.width {
            Some(w) => w as usize,
            None => {
                let mut w = rest / free;
                if first_free {
                    w += rest % free;
                    first_free = false;
                }
                w
            }
        });
    }
    Some(out)
}

fn push_run(runs: &mut Vec<(String, bool)>, text: String, bold: bool) {
    match runs.last_mut() {
        Some((t, b)) if *b == bold => t.push_str(&text),
        _ => runs.push((text, bold)),
    }
}

/// Lay a document out for a printer profile. The running row total is checked after every
/// element, before the next code is encoded or image decoded, so nothing is laid out past the
/// limit.
pub fn layout(d: &Doc, p: &Profile) -> Result<Layout, String> {
    let width = p.width_dots;
    let cols = (width / CELL_W) as usize;
    let mut ops: Vec<Op> = Vec::new();
    let mut rows: u32 = 0;
    let mut printed_since_cut = false;
    for (i, el) in d.content.iter().enumerate() {
        let at = |m: String| format!("receipt.content[{i}]: {m}");
        match el {
            Element::Text {
                text,
                align,
                bold,
                size,
            } => {
                let wcols = cols / usize::from(size.0);
                for line in wrap(text, wcols) {
                    ops.push(Op::Row {
                        runs: vec![(pad(&line, wcols, *align, false), *bold)],
                        w: size.0,
                        h: size.1,
                    });
                    rows += pitch(size.1);
                }
                printed_since_cut = true;
            }
            Element::Columns { cells, size } => {
                let wcols = cols / usize::from(size.0);
                let widths = cell_widths(cells, wcols)
                    .ok_or_else(|| at(format!("columns do not fit {wcols} characters")))?;
                let wrapped: Vec<Vec<String>> = cells
                    .iter()
                    .zip(&widths)
                    .map(|(c, w)| wrap(&normalize(&c.text), *w))
                    .collect();
                let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
                for r in 0..height {
                    let mut runs: Vec<(String, bool)> = Vec::new();
                    for (j, c) in cells.iter().enumerate() {
                        if j > 0 {
                            push_run(&mut runs, " ".into(), false);
                        }
                        let line = wrapped[j].get(r).map_or("", String::as_str);
                        push_run(&mut runs, pad(line, widths[j], c.align, true), c.bold);
                    }
                    ops.push(Op::Row {
                        runs,
                        w: size.0,
                        h: size.1,
                    });
                    rows += pitch(size.1);
                }
                printed_since_cut = true;
            }
            Element::Divider { ch } => {
                ops.push(Op::Row {
                    runs: vec![(ch.to_string().repeat(cols), false)],
                    w: 1,
                    h: 1,
                });
                rows += pitch(1);
                printed_since_cut = true;
            }
            Element::Image {
                bytes,
                align,
                width_percent,
                dither,
            } => {
                let gray = decode(bytes, MAX_IMAGE_PIXELS, MAX_IMAGE_ALLOC).map_err(at)?;
                let (w, h) = gray.dimensions();
                let tw = match width_percent {
                    Some(pc) => (u64::from(width) * u64::from(*pc) / 100).max(1) as u32,
                    None => w.min(width),
                };
                let th = ((u64::from(h) * u64::from(tw) + u64::from(w) / 2) / u64::from(w)).max(1);
                // before the resize: a tall image is refused while it is still small
                if th > u64::from(MAX_ROWS - rows.min(MAX_ROWS)) {
                    return Err(too_long());
                }
                let th = th as u32;
                let mut g = if (tw, th) == (w, h) {
                    gray
                } else {
                    resize_gray(&gray, tw, th, MAX_IMAGE_ALLOC).map_err(at)?
                };
                // image 0.25.10 `dither` indexes `x + 1` and would panic on a 1 pixel wide image
                if *dither && tw >= 2 && th >= 2 {
                    imageops::dither(&mut g, &imageops::BiLevel);
                }
                let bm = Bitmap::from_gray(&g);
                rows += bm.h;
                ops.push(Op::Image {
                    x: x_of(*align, width, tw),
                    bm,
                });
                printed_since_cut = true;
            }
            Element::Qr {
                data,
                ec,
                size,
                align,
            } => {
                let bm = codes::qr(data, *ec, *size, width).map_err(at)?;
                rows += bm.h;
                ops.push(Op::Image {
                    x: x_of(*align, width, bm.w),
                    bm,
                });
                printed_since_cut = true;
            }
            Element::Barcode {
                format,
                data,
                height,
                size,
                text,
                align,
            } => {
                let bm = codes::barcode(*format, data, *height, *size, width).map_err(at)?;
                rows += bm.h;
                let label = match format {
                    BarFormat::Ean13 => codes::ean13_digits(data).map_err(at)?,
                    BarFormat::Code128 => data.clone(),
                };
                let x = x_of(*align, width, bm.w);
                // the middle of the bars, in dots: the text is centred under them, not on the paper
                let mid = x + bm.w / 2;
                ops.push(Op::Image { x, bm });
                if *text {
                    // A number can be longer than the line (set C packs two digits in 11
                    // modules): hard chunks of one line, never a break at a space, which is data
                    let chars: Vec<char> = label.chars().collect();
                    for chunk in chars.chunks(cols.max(1)) {
                        let len = chunk.len();
                        let half = len as u32 * CELL_W / 2;
                        let left = ((mid.saturating_sub(half) / CELL_W) as usize).min(cols - len);
                        let mut line = " ".repeat(left);
                        line.extend(chunk);
                        ops.push(Op::Row {
                            runs: vec![(line, false)],
                            w: 1,
                            h: 1,
                        });
                        rows += pitch(1);
                    }
                }
                printed_since_cut = true;
            }
            Element::Feed(n) => {
                ops.push(Op::Feed(*n));
                rows += FEED_DOTS * u32::from(*n);
                printed_since_cut = true;
            }
            Element::Cut => {
                ops.push(Op::Cut);
                rows += cut_feed(p);
                printed_since_cut = false;
            }
            Element::Drawer(pin) => ops.push(Op::Drawer(*pin)),
        }
        if rows > MAX_ROWS {
            return Err(too_long());
        }
    }
    // a cut is added only when something was printed after the last one: opening the till alone
    // feeds and cuts nothing
    if d.cut && printed_since_cut {
        ops.push(Op::Cut);
        rows += cut_feed(p);
        if rows > MAX_ROWS {
            return Err(too_long());
        }
    }
    Ok(Layout {
        width_dots: width,
        ops,
    })
}

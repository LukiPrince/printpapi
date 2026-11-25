// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Dots: the canvas the drawn rows, images and codes go on, and its two outputs besides ESC/POS,
//! the preview PNG and the PDF. `image::codecs::png::PngEncoder`:
//! <https://docs.rs/image/0.25.10/image/codecs/png/struct.PngEncoder.html>.

use super::font::{CELL_H, CELL_W, font};
use super::layout::{Layout, Op};
use super::{Profile, pitch};
use crate::label::{Bitmap, PdfPage, mm_to_pt, pdf_pages};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

/// What a cut takes in the preview, and what a feed line, in dots.
const CUT_ROWS: u32 = 40;
const DRAWER_ROWS: u32 = 30;
const FEED_ROWS: u32 = 30;
/// White border around the paper in the preview.
const MARGIN: u32 = 16;
/// Paper at 8 dots a millimetre; the PDF keeps 4 mm above and below the receipt.
const DOTS_PER_MM: f64 = 8.0;
const PDF_PAD_MM: f64 = 4.0;

pub fn blank(w: u32, h: u32) -> Bitmap {
    let bytes_per_row = (w as usize).div_ceil(8);
    Bitmap {
        w,
        h,
        bytes_per_row,
        data: vec![0; bytes_per_row * h as usize],
    }
}

/// Black a rectangle, clipped to the bitmap.
pub fn fill(bm: &mut Bitmap, x: u32, y: u32, w: u32, h: u32) {
    for yy in y..(y + h).min(bm.h) {
        for xx in x..(x + w).min(bm.w) {
            bm.data[yy as usize * bm.bytes_per_row + xx as usize / 8] |= 0x80 >> (xx % 8);
        }
    }
}

fn get(bm: &Bitmap, x: u32, y: u32) -> bool {
    bm.data[y as usize * bm.bytes_per_row + x as usize / 8] & (0x80 >> (x % 8)) != 0
}

/// Draw a text row with its top at `y`: each dot of a glyph becomes a `w` x `h` block.
fn draw_row(bm: &mut Bitmap, y: u32, runs: &[(String, bool)], w: u8, h: u8) {
    let (w, h) = (u32::from(w), u32::from(h));
    let mut cell = 0u32;
    for (text, bold) in runs {
        let f = font(*bold);
        for c in text.chars() {
            let x0 = cell * CELL_W * w;
            if x0 >= bm.w {
                return;
            }
            for (gy, bits) in f.glyph(c).iter().enumerate() {
                for gx in 0..CELL_W {
                    if bits & (0x8000 >> gx) != 0 {
                        fill(bm, x0 + gx * w, y + gy as u32 * h, w, h);
                    }
                }
            }
            cell += 1;
        }
    }
}

/// The height of one op on the canvas, in dots.
pub fn op_height(op: &Op) -> u32 {
    match op {
        Op::Row { h, .. } => pitch(*h),
        Op::Image { bm, .. } => bm.h,
        Op::Feed(n) => FEED_ROWS * u32::from(*n),
        Op::Cut => CUT_ROWS,
        Op::Drawer(_) => DRAWER_ROWS,
    }
}

/// Rows and images one under the other on a canvas of `width` dots; feeds, cuts and drawers are
/// blank space (the stream never hands the last two here).
pub fn draw_ops(width: u32, ops: &[&Op]) -> Bitmap {
    let total: u32 = ops.iter().map(|o| op_height(o)).sum();
    let mut bm = blank(width, total);
    let mut y = 0;
    for op in ops {
        match op {
            Op::Row { runs, w, h } => draw_row(&mut bm, y, runs, *w, *h),
            Op::Image { x, bm: img } => {
                for yy in 0..img.h {
                    for xx in 0..img.w {
                        if get(img, xx, yy) && x + xx < width {
                            fill(&mut bm, x + xx, y + yy, 1, 1);
                        }
                    }
                }
            }
            Op::Feed(_) | Op::Cut | Op::Drawer(_) => {}
        }
        y += op_height(op);
    }
    bm
}

/// The whole receipt as one PNG: the paper on a white border, a cut as a dashed line, a drawer
/// as a grey row. With the `raster` code page the dots are those the printer gets.
pub fn preview_png(l: &Layout) -> Vec<u8> {
    let ops: Vec<&Op> = l.ops.iter().collect();
    let dots = draw_ops(l.width_dots, &ops);
    let (w, h) = (l.width_dots + 2 * MARGIN, dots.h + 2 * MARGIN);
    let mut gray = vec![255u8; (w * h) as usize];
    for y in 0..dots.h {
        for x in 0..dots.w {
            if get(&dots, x, y) {
                gray[((y + MARGIN) * w + x + MARGIN) as usize] = 0;
            }
        }
    }
    let mut y = 0;
    for op in &l.ops {
        match op {
            Op::Cut => {
                let line = (y + CUT_ROWS / 2 + MARGIN) * w;
                for x in 0..l.width_dots {
                    // 8 dots grey, 8 dots white
                    if x % 16 < 8 {
                        gray[(line + x + MARGIN) as usize] = 128;
                    }
                }
            }
            Op::Drawer(_) => {
                let mut bm = blank(l.width_dots, CELL_H);
                draw_row(&mut bm, 0, &[("[cash drawer]".to_string(), false)], 1, 1);
                for yy in 0..CELL_H {
                    for x in 0..l.width_dots {
                        if get(&bm, x, yy) {
                            gray[((y + yy + MARGIN) * w + x + MARGIN) as usize] = 128;
                        }
                    }
                }
            }
            _ => {}
        }
        y += op_height(op);
    }
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(&gray, w, h, ExtendedColorType::L8)
        .expect("a PNG into memory");
    out
}

/// The receipt as a PDF, one page per section between cuts that has a row or an image. Pages are
/// as wide as the paper and always taller than wide: the bitmap is centred at 8 dots a millimetre
/// with 4 mm above and below, the page at least `paper_mm + 1` mm tall, white at the bottom. The
/// server never picks an orientation (a wide page is turned by SumatraPDF), and a drawer does
/// nothing here.
pub fn pdf(l: &Layout, p: &Profile) -> Result<Vec<u8>, String> {
    let mut sections: Vec<Vec<&Op>> = vec![Vec::new()];
    for op in &l.ops {
        match op {
            Op::Cut => sections.push(Vec::new()),
            Op::Drawer(_) => {}
            op => sections.last_mut().expect("one section").push(op),
        }
    }
    let bitmaps: Vec<Bitmap> = sections
        .iter()
        .filter(|s| {
            s.iter()
                .any(|o| matches!(o, Op::Row { .. } | Op::Image { .. }))
        })
        .map(|s| draw_ops(l.width_dots, s))
        .collect();
    if bitmaps.is_empty() {
        return Err("receipt prints nothing on a PDF printer".into());
    }
    let paper = f64::from(p.paper_mm);
    let pages: Vec<PdfPage<'_>> = bitmaps
        .iter()
        .map(|bm| {
            let (w_mm, h_mm) = (f64::from(bm.w) / DOTS_PER_MM, f64::from(bm.h) / DOTS_PER_MM);
            let page_h = (h_mm + 2.0 * PDF_PAD_MM).max(paper + 1.0);
            PdfPage {
                w_pt: mm_to_pt(paper),
                h_pt: mm_to_pt(page_h),
                bitmap: bm,
                place: (
                    mm_to_pt((paper - w_mm) / 2.0),
                    mm_to_pt(page_h - PDF_PAD_MM - h_mm),
                    mm_to_pt(w_mm),
                    mm_to_pt(h_mm),
                ),
            }
        })
        .collect();
    Ok(pdf_pages(&pages))
}

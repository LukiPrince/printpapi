// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Label templates: render an image or a text to one label, as a PDF (printers that take PDF)
//! or as ZPL (raw label printers). Pure: bytes in, bytes out, no IO, no clock.
//!
//! Sources for the foreign formats this module writes:
//! - ZPL `^GF` (graphic field, ASCII hex, at most 99999 bytes per field):
//!   <https://docs.zebra.com/us/en/printers/software/zpl-pg/c-zpl-zpl-commands/r-zpl-gf.html>
//! - ZPL `^CI28` (UTF-8 text in `^FD`):
//!   <https://docs.zebra.com/us/en/printers/software/zpl-pg/c-zpl-zpl-commands/r-zpl-ci.html>
//! - `image` 0.25 (`ImageReader`, `Limits`, `imageops`): <https://docs.rs/image/0.25.10/image/>
//! - PDF 1.4 image XObjects and the classic xref table: ISO 32000-1, 7.5.4 and 8.9.5.

use crate::packing_slip::esc;
use image::{DynamicImage, GrayImage, ImageDecoder, ImageReader, Limits, imageops};
use serde_json::{Map, Value, json};
use std::io::Cursor;

/// Longest edge of the label, in printer dots.
pub const MAX_DOTS: u32 = 8000;
/// Longest text one job may render, in characters (after cleaning).
pub const MAX_TEXT: usize = 500;
/// Largest image we decode: 40 megapixels, 512 MiB of decoder memory.
const MAX_PIXELS: u64 = 40_000_000;
const MAX_ALLOC: u64 = 512 * 1024 * 1024;
/// Longest edge of a decoded image. The area cap alone would let a 40 000 000 x 1 image through.
const MAX_EDGE: u32 = 20_000;
/// `imageops::resize` first resamples into an Rgba32F buffer of source width x target height,
/// 16 bytes a pixel (image 0.25.10, `imageops/sample.rs`, `vertical_sample`). That buffer is not
/// covered by `Limits`, and a failed allocation aborts the whole process.
const RESIZE_BYTES_PER_PIXEL: u64 = 16;
/// A placed image never grows beyond this many dots per edge or in total, so a template cannot
/// make the resize allocate gigabytes. The label itself is at most `MAX_DOTS` per edge.
const MAX_PLACE_EDGE: f64 = 2.0 * MAX_DOTS as f64;
const MAX_PLACE_AREA: f64 = 64_000_000.0;
/// `^GF` takes at most 99999 bytes of data per field.
const GF_MAX_BYTES: usize = 99_999;
/// Luma below this prints black.
const THRESHOLD: u8 = 140;
/// Line pitch of text, as a multiple of the font height.
const LINE_PITCH: f64 = 1.1;
/// Zebra documents 10 dots as the smallest height and width of a scalable font.
const MIN_FONT: f64 = 10.0;
/// Average advance of Zebra's font 0 at the 0.72 width ratio, in font heights.
const ZPL_ADVANCE: f64 = 0.42;
/// Helvetica Bold capitals run about 0.72 em, digits 0.56. Budgeting for capitals keeps a line of
/// them inside its box; a run of the widest glyphs (W, M) can still overshoot.
const PDF_ADVANCE: f64 = 0.72;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    Image,
    Text,
}

impl Content {
    fn as_str(self) -> &'static str {
        match self {
            Content::Image => "image",
            Content::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    Pdf,
    Zpl,
}

/// One validated label template. Millimetres are the stored unit, dots are derived with `dpi`.
#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub name: String,
    pub content: Content,
    pub label_w_mm: f64,
    pub label_h_mm: f64,
    pub dpi: u32,
    /// 0, 90, 180 or 270 degrees clockwise.
    pub rotate: u32,
    pub crop_t: f64,
    pub crop_r: f64,
    pub crop_b: f64,
    pub crop_l: f64,
    pub place_x: f64,
    pub place_y: f64,
    pub place_w: f64,
    pub place_h: f64,
    pub wrap_chars: usize,
    pub lines_per_label: usize,
    pub darkness: Option<i64>,
    pub size_ref: Option<String>,
}

// ---------------------------------------------------------------- spec

fn only_keys(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown field in {what}: {k}")),
        None => Ok(()),
    }
}

fn get_obj<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a Map<String, Value>, String> {
    m.get(key)
        .ok_or_else(|| format!("{key} is required"))?
        .as_object()
        .ok_or_else(|| format!("{key} must be an object"))
}

fn get_num(m: &Map<String, Value>, key: &str, what: &str) -> Result<f64, String> {
    m.get(key)
        .ok_or_else(|| format!("{what}.{key} is required"))?
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{what}.{key} must be a number"))
}

fn get_int(m: &Map<String, Value>, key: &str, what: &str) -> Result<i64, String> {
    m.get(key)
        .ok_or_else(|| format!("{what} is required"))?
        .as_i64()
        .ok_or_else(|| format!("{what} must be an integer"))
}

fn dots_of(mm: f64, dpi: u32) -> f64 {
    mm / 25.4 * dpi as f64
}

/// Parse and validate the template JSON of the HTTP contract. Unknown fields are an error.
pub fn parse_spec(v: &Map<String, Value>) -> Result<Spec, String> {
    only_keys(
        v,
        &[
            "name",
            "content",
            "label_mm",
            "dpi",
            "rotate",
            "crop_percent",
            "place_mm",
            "text",
            "zpl_darkness",
            "size_ref",
        ],
        "template",
    )?;
    let name = v
        .get("name")
        .ok_or("name is required")?
        .as_str()
        .filter(|s| crate::dispatch::valid_option_value(s))
        .ok_or("name must be 1-64 chars of letters/digits/space/._-")?
        .to_string();
    let content = match v.get("content").ok_or("content is required")?.as_str() {
        Some("image") => Content::Image,
        Some("text") => Content::Text,
        _ => return Err("content must be \"image\" or \"text\"".into()),
    };

    let dpi = get_int(v, "dpi", "dpi")?;
    if !(100..=600).contains(&dpi) {
        return Err("dpi must be 100-600".into());
    }
    let dpi = dpi as u32;
    let rotate = get_int(v, "rotate", "rotate")?;
    if ![0, 90, 180, 270].contains(&rotate) {
        return Err("rotate must be 0, 90, 180 or 270".into());
    }

    let lm = get_obj(v, "label_mm")?;
    only_keys(lm, &["w", "h"], "label_mm")?;
    let (label_w_mm, label_h_mm) = (get_num(lm, "w", "label_mm")?, get_num(lm, "h", "label_mm")?);
    for (mm, axis) in [(label_w_mm, "w"), (label_h_mm, "h")] {
        let d = dots_of(mm, dpi).round();
        if !(mm > 0.0 && (1.0..=MAX_DOTS as f64).contains(&d)) {
            return Err(format!(
                "label_mm.{axis} must be 1-{MAX_DOTS} dots at {dpi} dpi"
            ));
        }
    }

    let cp = get_obj(v, "crop_percent")?;
    only_keys(cp, &["t", "r", "b", "l"], "crop_percent")?;
    let (crop_t, crop_r, crop_b, crop_l) = (
        get_num(cp, "t", "crop_percent")?,
        get_num(cp, "r", "crop_percent")?,
        get_num(cp, "b", "crop_percent")?,
        get_num(cp, "l", "crop_percent")?,
    );
    if ![crop_t, crop_r, crop_b, crop_l]
        .iter()
        .all(|c| (0.0..=95.0).contains(c))
    {
        return Err("crop_percent values must be 0-95".into());
    }
    if crop_t + crop_b >= 100.0 || crop_l + crop_r >= 100.0 {
        return Err("crop_percent leaves nothing of the image".into());
    }

    let pm = get_obj(v, "place_mm")?;
    only_keys(pm, &["x", "y", "w", "h"], "place_mm")?;
    let (place_x, place_y, place_w, place_h) = (
        get_num(pm, "x", "place_mm")?,
        get_num(pm, "y", "place_mm")?,
        get_num(pm, "w", "place_mm")?,
        get_num(pm, "h", "place_mm")?,
    );
    if place_w <= 0.0 || place_h <= 0.0 {
        return Err("place_mm.w and place_mm.h must be greater than 0".into());
    }
    let (pw, ph) = (
        dots_of(place_w, dpi).round().max(1.0),
        dots_of(place_h, dpi).round().max(1.0),
    );
    if pw > MAX_PLACE_EDGE || ph > MAX_PLACE_EDGE || pw * ph > MAX_PLACE_AREA {
        return Err("place_mm is too large".into());
    }
    // ponytail: the origin may sit anywhere up to a million dots off the label; beyond that the
    // arithmetic is not worth guarding and the content is invisible anyway.
    if dots_of(place_x, dpi).abs() > 1e6 || dots_of(place_y, dpi).abs() > 1e6 {
        return Err("place_mm.x and place_mm.y are too far from the label".into());
    }

    let (mut wrap_chars, mut lines_per_label) = (0, 0);
    if let Some(t) = v.get("text") {
        let t = t.as_object().ok_or("text must be an object")?;
        only_keys(t, &["wrap_chars", "lines_per_label"], "text")?;
        for (key, slot) in [
            ("wrap_chars", &mut wrap_chars),
            ("lines_per_label", &mut lines_per_label),
        ] {
            if t.contains_key(key) {
                let n = get_int(t, key, &format!("text.{key}"))?;
                if !(0..=200).contains(&n) {
                    return Err(format!("text.{key} must be 0-200"));
                }
                *slot = n as usize;
            }
        }
    }

    let darkness = match v.get("zpl_darkness") {
        None => None,
        Some(_) => {
            let n = get_int(v, "zpl_darkness", "zpl_darkness")?;
            if !(-30..=30).contains(&n) {
                return Err("zpl_darkness must be -30 to 30".into());
            }
            Some(n)
        }
    };
    let size_ref = match v.get("size_ref") {
        None => None,
        Some(s) => Some(
            s.as_str()
                .filter(|s| s.chars().count() <= 64)
                .ok_or("size_ref must be a string of at most 64 chars")?
                .to_string(),
        ),
    };

    Ok(Spec {
        name,
        content,
        label_w_mm,
        label_h_mm,
        dpi,
        rotate: rotate as u32,
        crop_t,
        crop_r,
        crop_b,
        crop_l,
        place_x,
        place_y,
        place_w,
        place_h,
        wrap_chars,
        lines_per_label,
        darkness,
        size_ref,
    })
}

/// The canonical contract form of a template.
pub fn spec_json(s: &Spec) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(s.name));
    m.insert("content".into(), json!(s.content.as_str()));
    m.insert(
        "label_mm".into(),
        json!({"w": s.label_w_mm, "h": s.label_h_mm}),
    );
    m.insert("dpi".into(), json!(s.dpi));
    m.insert("rotate".into(), json!(s.rotate));
    m.insert(
        "crop_percent".into(),
        json!({"t": s.crop_t, "r": s.crop_r, "b": s.crop_b, "l": s.crop_l}),
    );
    m.insert(
        "place_mm".into(),
        json!({"x": s.place_x, "y": s.place_y, "w": s.place_w, "h": s.place_h}),
    );
    m.insert(
        "text".into(),
        json!({"wrap_chars": s.wrap_chars, "lines_per_label": s.lines_per_label}),
    );
    if let Some(d) = s.darkness {
        m.insert("zpl_darkness".into(), json!(d));
    }
    if let Some(r) = &s.size_ref {
        m.insert("size_ref".into(), json!(r));
    }
    Value::Object(m)
}

fn label_dots(s: &Spec) -> (u32, u32) {
    (
        dots_of(s.label_w_mm, s.dpi).round() as u32,
        dots_of(s.label_h_mm, s.dpi).round() as u32,
    )
}

/// Where the content sits on the label, in dots: x, y (may be negative), w, h.
fn place_dots(s: &Spec) -> (f64, f64, f64, f64) {
    (
        dots_of(s.place_x, s.dpi).round(),
        dots_of(s.place_y, s.dpi).round(),
        dots_of(s.place_w, s.dpi).round().max(1.0),
        dots_of(s.place_h, s.dpi).round().max(1.0),
    )
}

// ---------------------------------------------------------------- bitmap

/// A 1-bit label: 1 = black, most significant bit first, rows padded to whole bytes.
#[derive(Debug)]
struct Bitmap {
    w: u32,
    h: u32,
    bytes_per_row: usize,
    data: Vec<u8>,
}

impl Bitmap {
    fn from_gray(g: &GrayImage) -> Bitmap {
        let (w, h) = g.dimensions();
        let bytes_per_row = (w as usize).div_ceil(8);
        let mut data = vec![0u8; bytes_per_row * h as usize];
        for (x, y, p) in g.enumerate_pixels() {
            if p.0[0] < THRESHOLD {
                data[y as usize * bytes_per_row + x as usize / 8] |= 0x80 >> (x % 8);
            }
        }
        Bitmap {
            w,
            h,
            bytes_per_row,
            data,
        }
    }

    #[cfg(test)]
    fn black(&self, x: u32, y: u32) -> bool {
        self.data[y as usize * self.bytes_per_row + x as usize / 8] & (0x80 >> (x % 8)) != 0
    }
}

/// Decode the first frame of a GIF, PNG or JPEG and flatten transparency onto white.
fn decode(bytes: &[u8]) -> Result<GrayImage, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("cannot read image: {e}"))?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_ALLOC);
    limits.max_image_width = Some(MAX_EDGE);
    limits.max_image_height = Some(MAX_EDGE);
    reader.limits(limits);
    // The decoder reads the header only; the size check comes before anything is allocated.
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("not a supported image (GIF, PNG or JPEG): {e}"))?;
    let (w, h) = decoder.dimensions();
    // (`limits` above already refuses an edge over MAX_EDGE while the decoder is built.)
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!(
            "image is too large ({w}x{h}, at most {MAX_PIXELS} pixels)"
        ));
    }
    // EXIF orientation, as browsers and phones show the picture (the editor's canvas does too).
    // Source: image 0.25.10 `ImageDecoder::orientation` and `DynamicImage::apply_orientation`.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    // A GIF decodes to its first frame only.
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("cannot decode image: {e}"))?;
    img.apply_orientation(orientation);
    let la = img.to_luma_alpha8();
    let mut out = GrayImage::new(la.width(), la.height());
    for (x, y, p) in la.enumerate_pixels() {
        let (l, a) = (u32::from(p.0[0]), u32::from(p.0[1]));
        out.put_pixel(
            x,
            y,
            image::Luma([((l * a + 255 * (255 - a) + 127) / 255) as u8]),
        );
    }
    Ok(out)
}

/// Decode, rotate, crop, scale and place an image on a white label, then threshold to 1 bit.
fn raster(s: &Spec, bytes: &[u8]) -> Result<Bitmap, String> {
    let gray = decode(bytes)?;
    let rotated = match s.rotate {
        90 => imageops::rotate90(&gray),
        180 => imageops::rotate180(&gray),
        270 => imageops::rotate270(&gray),
        _ => gray,
    };
    let (w, h) = rotated.dimensions();
    let cut = |len: u32, a: f64, b: f64| {
        let start = (f64::from(len) * a / 100.0).round() as u32;
        let end = len - (f64::from(len) * b / 100.0).round() as u32;
        let start = start.min(len - 1);
        (start, end.max(start + 1).min(len))
    };
    let (x0, x1) = cut(w, s.crop_l, s.crop_r);
    let (y0, y1) = cut(h, s.crop_t, s.crop_b);
    let cropped = imageops::crop_imm(&rotated, x0, y0, x1 - x0, y1 - y0).to_image();

    let (px, py, pw, ph) = place_dots(s);
    let (pw, ph) = (pw as u32, ph as u32);
    // Skipping a no-op resize keeps a 1-dot line exactly one dot wide.
    let placed = if cropped.dimensions() == (pw, ph) {
        cropped
    } else {
        if u64::from(cropped.width()) * u64::from(ph) * RESIZE_BYTES_PER_PIXEL > MAX_ALLOC {
            return Err("image is too large for this placement".into());
        }
        imageops::resize(&cropped, pw, ph, imageops::FilterType::Lanczos3)
    };

    let (lw, lh) = label_dots(s);
    let mut canvas = GrayImage::from_pixel(lw, lh, image::Luma([255]));
    let (ox, oy) = (px as i64, py as i64);
    for y in 0..i64::from(ph) {
        let ty = oy + y;
        if !(0..i64::from(lh)).contains(&ty) {
            continue;
        }
        for x in 0..i64::from(pw) {
            let tx = ox + x;
            if (0..i64::from(lw)).contains(&tx) {
                canvas.put_pixel(tx as u32, ty as u32, *placed.get_pixel(x as u32, y as u32));
            }
        }
    }
    Ok(Bitmap::from_gray(&canvas))
}

// ---------------------------------------------------------------- ZPL

fn zpl_header(s: &Spec) -> String {
    let (w, h) = label_dots(s);
    // ^LH stays set on the printer, so an earlier job could otherwise shift every ^FO here.
    let mut out = format!("^XA^PW{w}^LL{h}^LH0,0");
    if let Some(d) = s.darkness {
        out.push_str(&format!("^MD{d}"));
    }
    out
}

fn hex(data: &[u8], out: &mut String) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for &b in data {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 15)] as char);
    }
}

fn zpl_image(s: &Spec, bm: &Bitmap) -> Vec<u8> {
    let mut out = zpl_header(s);
    // One ^GF carries at most 99999 bytes, so a tall label goes out in horizontal bands.
    let rows_per_band = (GF_MAX_BYTES / bm.bytes_per_row).max(1);
    let mut y = 0usize;
    while y < bm.h as usize {
        let rows = rows_per_band.min(bm.h as usize - y);
        let n = rows * bm.bytes_per_row;
        out.push_str(&format!("^FO0,{y}^GFA,{n},{n},{},", bm.bytes_per_row));
        hex(
            &bm.data[y * bm.bytes_per_row..y * bm.bytes_per_row + n],
            &mut out,
        );
        out.push_str("^FS");
        y += rows;
    }
    out.push_str("^XZ");
    out.into_bytes()
}

// ---------------------------------------------------------------- PDF

/// Points with at most two decimals and no trailing zeros: `288`, `72.5`.
fn pt(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn mm_to_pt(mm: f64) -> f64 {
    mm * 72.0 / 25.4
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    o.extend_from_slice(data);
    o.extend_from_slice(b"\nendstream");
    o
}

/// Assemble numbered objects (object `i + 1` is `bodies[i]`) into a file with a classic xref
/// table. Object 1 must be the catalog.
fn assemble(bodies: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(bodies.len());
    for (i, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).as_bytes(),
    );
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn pdf_image(s: &Spec, bm: &Bitmap) -> Vec<u8> {
    let (pw, ph) = (pt(mm_to_pt(s.label_w_mm)), pt(mm_to_pt(s.label_h_mm)));
    // PDF DeviceGray: 0 is black, so the bits flip. Padding bits become 1 (white).
    let inverted: Vec<u8> = bm.data.iter().map(|b| !b).collect();
    let content = format!("q {pw} 0 0 {ph} 0 0 cm /Im0 Do Q");
    // ponytail: the image stays uncompressed (no deflate dependency); a 4x6 inch label at 203 dpi
    // is about 124 KB, and the job store holds it as is.
    assemble(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /XObject << /Im0 5 0 R >> >> /Contents 4 0 R >>")
            .into_bytes(),
        stream("", content.as_bytes()),
        stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceGray /BitsPerComponent 1",
                bm.w, bm.h
            ),
            &inverted,
        ),
    ])
}

// ---------------------------------------------------------------- rendering

/// Render an image (GIF, PNG or JPEG bytes) onto one label.
pub fn render_image(s: &Spec, bytes: &[u8], out: Output) -> Result<Vec<u8>, String> {
    let bm = raster(s, bytes)?;
    Ok(match out {
        Output::Zpl => zpl_image(s, &bm),
        Output::Pdf => pdf_image(s, &bm),
    })
}

/// Make text safe for a ZPL field: `^` and `~` become spaces, control characters go, whitespace
/// folds to single spaces.
pub fn clean_text(s: &str) -> String {
    let mapped: String = s
        .chars()
        .filter_map(|c| match c {
            '^' | '~' => Some(' '),
            c if c.is_whitespace() => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect();
    mapped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Greedy word wrap: a word moves to the next line when it would pass `wrap_chars`; a word longer
/// than the limit stays whole. 0 keeps everything on one line.
pub fn wrap_lines(text: &str, wrap_chars: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return vec![];
    }
    if wrap_chars == 0 {
        return vec![words.join(" ")];
    }
    let mut lines: Vec<String> = vec![];
    let mut cur = String::new();
    let mut len = 0;
    for w in words {
        let wl = w.chars().count();
        if cur.is_empty() {
            cur.push_str(w);
            len = wl;
        } else if len + 1 + wl <= wrap_chars {
            cur.push(' ');
            cur.push_str(w);
            len += 1 + wl;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(w);
            len = wl;
        }
    }
    lines.push(cur);
    lines
}

/// The font height that makes `lines` fill the box:
/// `min(box_h / (lines * 1.1), box_w / (longest * advance))`.
fn ideal_font(s: &Spec, lines: &[String], advance: f64) -> f64 {
    let (_, _, bw, bh) = place_dots(s);
    let longest = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    (bh / (lines.len() as f64 * LINE_PITCH)).min(bw / (longest * advance))
}

/// Font height in dots and the y of the top of the line block, for lines laid out in the box:
/// `ideal_font` within 10..=400, block centred.
fn text_layout(s: &Spec, lines: &[String], advance: f64) -> (f64, f64, f64, f64) {
    let (bx, by, _, bh) = place_dots(s);
    let n = lines.len() as f64;
    let h = ideal_font(s, lines, advance).clamp(MIN_FONT, 400.0).round();
    let top = by + (bh - n * LINE_PITCH * h) / 2.0;
    (h, top, bx, by)
}

/// Render text to one label per group of `lines_per_label` lines. ZPL labels are chained in
/// one byte string, PDF labels are pages. Rotation, crop and `dpi` scaling of images do not apply.
pub fn render_text(s: &Spec, text: &str, out: Output) -> Result<Vec<u8>, String> {
    let text = clean_text(text);
    if text.is_empty() {
        return Err("text is empty".into());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("text is longer than {MAX_TEXT} characters"));
    }
    let lines = wrap_lines(&text, s.wrap_chars);
    let per = if s.lines_per_label == 0 {
        lines.len()
    } else {
        s.lines_per_label
    };
    let groups: Vec<&[String]> = lines.chunks(per).collect();
    // Below the smallest font the block outgrows its box and lines would go missing without a
    // word. A refused job is one somebody sees; a label with half its text gets shipped.
    let advance = match out {
        Output::Zpl => ZPL_ADVANCE,
        Output::Pdf => PDF_ADVANCE,
    };
    if groups.iter().any(|g| ideal_font(s, g, advance) < MIN_FONT) {
        return Err("text does not fit the template, even at the smallest font".into());
    }
    match out {
        Output::Zpl => Ok(zpl_text(s, &groups)),
        Output::Pdf => Ok(pdf_text(s, &groups)),
    }
}

fn zpl_text(s: &Spec, groups: &[&[String]]) -> Vec<u8> {
    let mut out = String::new();
    for g in groups {
        let (h, top, bx, _) = text_layout(s, g, ZPL_ADVANCE);
        out.push_str(&zpl_header(s));
        // ^CI28 makes ^FD take UTF-8 (docs link in the module header). Only when needed: it stays
        // set on the printer, where it would change how later raw jobs read their bytes, and
        // older firmware does not have it.
        if !g.iter().all(|l| l.is_ascii()) {
            out.push_str("^CI28");
        }
        for (i, line) in g.iter().enumerate() {
            // ponytail: ^FO starts at 0 (Zebra's ^FO range is 0-32000), so a box that hangs off
            // the left edge starts its text at x=0 instead of being cut, and a line that starts
            // above the label or at/below its bottom edge is left out rather than stacked on y=0.
            let y = (top + i as f64 * LINE_PITCH * h).round();
            if y < 0.0 || y >= f64::from(label_dots(s).1) {
                continue;
            }
            let x = bx.max(0.0) as i64;
            let y = y as i64;
            out.push_str(&format!(
                "^FO{x},{y}^A0N,{},{}^FD{line}^FS",
                h as i64,
                (h * 0.72).round().max(MIN_FONT) as i64
            ));
        }
        out.push_str("^XZ");
    }
    out.into_bytes()
}

fn pdf_text(s: &Spec, groups: &[&[String]]) -> Vec<u8> {
    let (pw, ph) = (pt(mm_to_pt(s.label_w_mm)), mm_to_pt(s.label_h_mm));
    let scale = 72.0 / f64::from(s.dpi);
    let mut bodies = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        Vec::new(), // pages, filled below
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    ];
    let mut kids = String::new();
    for (i, g) in groups.iter().enumerate() {
        let (h, top, bx, _) = text_layout(s, g, PDF_ADVANCE);
        let mut content = Vec::new();
        for (j, line) in g.iter().enumerate() {
            // The baseline sits about 0.8 of the font height below the top of its line.
            let base = top + j as f64 * LINE_PITCH * h + 0.8 * h;
            content.extend_from_slice(
                format!(
                    "BT /F1 {} Tf {} {} Td (",
                    pt(h * scale),
                    pt(bx * scale),
                    pt(ph - base * scale)
                )
                .as_bytes(),
            );
            content.extend_from_slice(&esc(line));
            content.extend_from_slice(b") Tj ET\n");
        }
        let page = 4 + 2 * i;
        kids.push_str(&format!("{page} 0 R "));
        bodies.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {}] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
                pt(ph),
                page + 1
            )
            .into_bytes(),
        );
        bodies.push(stream("", &content));
    }
    bodies[1] = format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.trim_end(),
        groups.len()
    )
    .into_bytes();
    assemble(&bodies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::io::Cursor;

    fn base() -> Value {
        json!({
            "name": "shipping-4x6",
            "content": "image",
            "label_mm": { "w": 101.6, "h": 152.4 },
            "dpi": 203,
            "rotate": 90,
            "crop_percent": { "t": 0.0, "r": 0.0, "b": 14.3, "l": 0.0 },
            "place_mm": { "x": 0.8, "y": 1.3, "w": 100.1, "h": 149.8 },
            "text": { "wrap_chars": 12, "lines_per_label": 2 },
            "zpl_darkness": 30,
            "size_ref": "Standard:4x6"
        })
    }

    fn parse(v: &Value) -> Result<Spec, String> {
        parse_spec(v.as_object().expect("object"))
    }

    fn with(f: impl FnOnce(&mut Value)) -> Result<Spec, String> {
        let mut v = base();
        f(&mut v);
        parse(&v)
    }

    /// A spec on a 254 dpi grid, so one millimetre is exactly ten dots and tests can think in dots.
    fn dots(
        label: (u32, u32),
        place: (i32, i32, u32, u32),
        rotate: u32,
        crop: (f64, f64, f64, f64),
    ) -> Spec {
        let mm = |d: f64| d / 10.0;
        parse(&json!({
            "name": "t", "content": "image",
            "label_mm": { "w": mm(label.0 as f64), "h": mm(label.1 as f64) },
            "dpi": 254, "rotate": rotate,
            "crop_percent": { "t": crop.0, "r": crop.1, "b": crop.2, "l": crop.3 },
            "place_mm": { "x": mm(place.0 as f64), "y": mm(place.1 as f64),
                          "w": mm(place.2 as f64), "h": mm(place.3 as f64) }
        }))
        .expect("valid spec")
    }

    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba(f(x, y)));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .expect("encode");
        out.into_inner()
    }

    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    fn find(hay: &[u8], needle: &str) -> Option<usize> {
        hay.windows(needle.len())
            .position(|w| w == needle.as_bytes())
    }

    fn count(hay: &[u8], needle: &str) -> usize {
        hay.windows(needle.len())
            .filter(|w| *w == needle.as_bytes())
            .count()
    }

    // ---- spec ----

    #[test]
    fn spec_round_trips() {
        let spec = parse(&base()).expect("valid");
        assert_eq!(spec_json(&spec), base());
        assert_eq!(parse(&spec_json(&spec)).expect("valid again"), spec);
    }

    #[test]
    fn optional_fields_default() {
        let spec = with(|v| {
            let m = v.as_object_mut().unwrap();
            m.remove("text");
            m.remove("zpl_darkness");
            m.remove("size_ref");
        })
        .expect("valid");
        assert_eq!((spec.wrap_chars, spec.lines_per_label), (0, 0));
        assert_eq!(spec.darkness, None);
        let j = spec_json(&spec);
        assert_eq!(j["text"], json!({"wrap_chars": 0, "lines_per_label": 0}));
        assert!(j.get("zpl_darkness").is_none() && j.get("size_ref").is_none());
    }

    #[test]
    fn rejects_rotate_45() {
        assert!(with(|v| v["rotate"] = json!(45)).is_err());
        for r in [0, 90, 180, 270] {
            assert!(with(|v| v["rotate"] = json!(r)).is_ok());
        }
    }

    #[test]
    fn rejects_crop_of_everything() {
        assert!(
            with(|v| {
                v["crop_percent"] = json!({"t": 50.0, "r": 0.0, "b": 50.0, "l": 0.0});
            })
            .is_err()
        );
        assert!(
            with(|v| {
                v["crop_percent"] = json!({"t": 0.0, "r": 60.0, "b": 0.0, "l": 40.0});
            })
            .is_err()
        );
        assert!(
            with(|v| {
                v["crop_percent"] = json!({"t": 96.0, "r": 0.0, "b": 0.0, "l": 0.0});
            })
            .is_err()
        );
        assert!(
            with(|v| {
                v["crop_percent"] = json!({"t": -1.0, "r": 0.0, "b": 0.0, "l": 0.0});
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_label_over_max_dots() {
        // 1100 mm at 203 dpi is 8791 dots.
        assert!(with(|v| v["label_mm"]["w"] = json!(1100.0)).is_err());
        assert!(with(|v| v["label_mm"]["h"] = json!(0.0)).is_err());
        assert!(with(|v| v["label_mm"]["h"] = json!(0.01)).is_err()); // rounds to 0 dots
        assert!(with(|v| v["dpi"] = json!(99)).is_err());
        assert!(with(|v| v["dpi"] = json!(601)).is_err());
        assert!(with(|v| v["place_mm"]["w"] = json!(0.0)).is_err());
    }

    #[test]
    fn rejects_oversized_placement() {
        assert!(with(|v| v["place_mm"]["w"] = json!(100000.0)).is_err());
    }

    #[test]
    fn rejects_unknown_field() {
        assert!(with(|v| v["bogus"] = json!(1)).is_err());
        assert!(with(|v| v["place_mm"]["z"] = json!(1)).is_err());
        assert!(with(|v| v["text"]["x"] = json!(1)).is_err());
        assert!(with(|v| v["crop_percent"]["x"] = json!(1)).is_err());
        assert!(with(|v| v["label_mm"]["x"] = json!(1)).is_err());
    }

    #[test]
    fn rejects_bad_names_and_ranges() {
        assert!(with(|v| v["name"] = json!("")).is_err());
        assert!(with(|v| v["name"] = json!("a".repeat(65))).is_err());
        assert!(with(|v| v["name"] = json!("bad/name")).is_err());
        assert!(with(|v| v["content"] = json!("video")).is_err());
        assert!(with(|v| v["zpl_darkness"] = json!(31)).is_err());
        assert!(with(|v| v["text"]["wrap_chars"] = json!(201)).is_err());
        assert!(with(|v| v["size_ref"] = json!("x".repeat(65))).is_err());
        assert!(
            with(|v| {
                v.as_object_mut().unwrap().remove("dpi");
            })
            .is_err()
        );
    }

    // ---- image pipeline ----

    #[test]
    fn rotate_90_moves_left_edge_to_top() {
        // 40x20 image, left column black; rotated clockwise it is 20x40 with the top row black.
        let s = dots((20, 40), (0, 0, 20, 40), 90, (0.0, 0.0, 0.0, 0.0));
        let bytes = png(40, 20, |x, _| if x == 0 { BLACK } else { WHITE });
        let b = raster(&s, &bytes).expect("raster");
        assert_eq!((b.w, b.h), (20, 40));
        assert!((0..20).all(|x| b.black(x, 0)));
        assert!((0..20).all(|x| !b.black(x, 1) && !b.black(x, 39)));
    }

    #[test]
    fn crop_removes_the_bottom_strip() {
        // 20x40 image, the lower quarter is white. Cropping it off leaves a solid black 20x30.
        let s = dots((20, 30), (0, 0, 20, 30), 0, (0.0, 0.0, 25.0, 0.0));
        let bytes = png(20, 40, |_, y| if y < 30 { BLACK } else { WHITE });
        let b = raster(&s, &bytes).expect("raster");
        assert!((0..30).all(|y| (0..20).all(|x| b.black(x, y))));
    }

    #[test]
    fn native_size_keeps_a_one_dot_line() {
        let s = dots((30, 30), (0, 0, 30, 30), 0, (0.0, 0.0, 0.0, 0.0));
        let bytes = png(30, 30, |x, _| if x == 7 { BLACK } else { WHITE });
        let b = raster(&s, &bytes).expect("raster");
        for y in 0..30 {
            for x in 0..30 {
                assert_eq!(b.black(x, y), x == 7, "dot {x},{y}");
            }
        }
    }

    #[test]
    fn placement_off_the_label_is_clipped() {
        // Label 20x20; the 20x20 black image sits at (-10, 10): the visible part is x<10, y>=10.
        let s = dots((20, 20), (-10, 10, 20, 20), 0, (0.0, 0.0, 0.0, 0.0));
        let b = raster(&s, &png(20, 20, |_, _| BLACK)).expect("raster");
        for y in 0..20 {
            for x in 0..20 {
                assert_eq!(b.black(x, y), x < 10 && y >= 10, "dot {x},{y}");
            }
        }
        // Entirely off the label: blank, no panic.
        let s = dots((20, 20), (500, -500, 20, 20), 0, (0.0, 0.0, 0.0, 0.0));
        let b = raster(&s, &png(20, 20, |_, _| BLACK)).expect("raster");
        assert!((0..20).all(|y| (0..20).all(|x| !b.black(x, y))));
    }

    #[test]
    fn transparent_pixels_print_white() {
        let s = dots((20, 10), (0, 0, 20, 10), 0, (0.0, 0.0, 0.0, 0.0));
        let bytes = png(20, 10, |x, _| if x < 10 { [0, 0, 0, 0] } else { BLACK });
        let b = raster(&s, &bytes).expect("raster");
        for y in 0..10 {
            for x in 0..20 {
                assert_eq!(b.black(x, y), x >= 10, "dot {x},{y}");
            }
        }
    }

    #[test]
    fn first_frame_of_an_animated_gif() {
        use image::codecs::gif::GifEncoder;
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            let black = image::RgbaImage::from_pixel(20, 10, image::Rgba(BLACK));
            let white = image::RgbaImage::from_pixel(20, 10, image::Rgba(WHITE));
            enc.encode_frame(image::Frame::new(black)).expect("frame 1");
            enc.encode_frame(image::Frame::new(white)).expect("frame 2");
        }
        let s = dots((20, 10), (0, 0, 20, 10), 0, (0.0, 0.0, 0.0, 0.0));
        let b = raster(&s, &out).expect("raster");
        assert!((0..10).all(|y| (0..20).all(|x| b.black(x, y))));
    }

    #[test]
    fn huge_header_is_refused() {
        // A real tiny PNG whose IHDR claims 10000x10000 (100 Mpx); the CRC is fixed up so the
        // decoder reaches the size check instead of failing on the checksum.
        let mut bytes = png(2, 2, |_, _| BLACK);
        bytes[16..20].copy_from_slice(&10000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&10000u32.to_be_bytes());
        let crc = crc32(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        let s = dots((20, 20), (0, 0, 20, 20), 0, (0.0, 0.0, 0.0, 0.0));
        let e = raster(&s, &bytes).expect_err("must refuse");
        assert!(e.contains("too large") || e.contains("limit"), "{e}");
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
        }
        !c
    }

    #[test]
    fn resize_buffer_is_bounded() {
        // image 0.25 resamples vertically first into an Rgba32F buffer of source width x target
        // height x 16 bytes (imageops/sample.rs, vertical_sample). 8000 x 5000 x 16 is 640 MB,
        // past MAX_ALLOC, and a failed allocation aborts the process instead of panicking.
        let s = dots((100, 100), (0, 0, 12800, 5000), 0, (0.0, 0.0, 0.0, 0.0));
        let img = image::GrayImage::from_pixel(8000, 1, image::Luma([0]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .expect("encode");
        let e = raster(&s, &out.into_inner()).expect_err("must refuse");
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn extreme_edge_is_refused_from_the_header() {
        // 40 Mpx is fine by area, but a 30000 x 1 image has no business here.
        let mut bytes = png(2, 2, |_, _| BLACK);
        bytes[16..20].copy_from_slice(&30000u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_be_bytes());
        let crc = crc32(&bytes[12..29]);
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        let s = dots((20, 20), (0, 0, 20, 20), 0, (0.0, 0.0, 0.0, 0.0));
        let e = raster(&s, &bytes).expect_err("must refuse");
        assert!(e.contains("too large") || e.contains("limit"), "{e}");
    }

    #[test]
    fn exif_orientation_is_applied() {
        use image::ImageEncoder;
        // 32x16 JPEG, the left 8 columns black. EXIF orientation 6 says "rotate 90 clockwise to
        // display", so the upright image is 16x32 with the black strip along the top.
        // Tiff header, one IFD entry (tag 0x0112, SHORT, 1, value 6), no next IFD.
        let exif = vec![
            0x49, 0x49, 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            0,
        ];
        let gray =
            image::GrayImage::from_fn(32, 16, |x, _| image::Luma([if x < 8 { 0 } else { 255 }]));
        let mut jpeg = Vec::new();
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100);
        enc.set_exif_metadata(exif).expect("exif");
        enc.write_image(gray.as_raw(), 32, 16, image::ExtendedColorType::L8)
            .expect("encode");
        let s = dots((16, 32), (0, 0, 16, 32), 0, (0.0, 0.0, 0.0, 0.0));
        let b = raster(&s, &jpeg).expect("raster");
        assert_eq!((b.w, b.h), (16, 32));
        assert!((0..16).all(|x| b.black(x, 3)), "top strip is black");
        assert!((0..16).all(|x| !b.black(x, 20)), "the rest is white");
    }

    #[test]
    fn garbage_is_an_error() {
        let s = dots((20, 20), (0, 0, 20, 20), 0, (0.0, 0.0, 0.0, 0.0));
        assert!(render_image(&s, b"definitely not an image", Output::Zpl).is_err());
        assert!(render_image(&s, b"", Output::Pdf).is_err());
    }

    // ---- outputs ----

    fn ship_label_spec() -> Spec {
        let mut v = base();
        v["rotate"] = json!(0);
        v["text"] = json!({"wrap_chars": 0, "lines_per_label": 0});
        v["crop_percent"] = json!({"t": 0.0, "r": 0.0, "b": 0.0, "l": 0.0});
        v["place_mm"] = json!({"x": 0.0, "y": 0.0, "w": 101.6, "h": 152.4});
        parse(&v).expect("valid")
    }

    fn noisy(w: u32, h: u32) -> Vec<u8> {
        png(w, h, |x, y| {
            if (x * 7 + y * 13) % 5 < 2 {
                BLACK
            } else {
                WHITE
            }
        })
    }

    #[test]
    fn zpl_bands_stay_under_the_gf_limit() {
        let s = ship_label_spec();
        let zpl = render_image(&s, &noisy(812, 1218), Output::Zpl).expect("zpl");
        let text = String::from_utf8(zpl).expect("ascii");
        let mut rows = 0;
        let mut bands = 0;
        for part in text.split("^GFA,").skip(1) {
            let head: Vec<&str> = part.splitn(5, ',').collect();
            let (b, c, row) = (
                head[0].parse::<usize>().expect("b"),
                head[1].parse::<usize>().expect("c"),
                head[2].parse::<usize>().expect("row"),
            );
            assert!(b <= 99999 && b == c, "band of {b} bytes");
            assert_eq!(row, 102);
            assert_eq!(b % row, 0);
            let hex = head[3].split("^FS").next().expect("data");
            assert_eq!(hex.len(), b * 2);
            assert!(
                hex.bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
            );
            rows += b / row;
            bands += 1;
        }
        assert_eq!(rows, 1218);
        assert!(bands >= 2);
        assert!(text.ends_with("^XZ"));
    }

    #[test]
    fn zpl_has_label_size() {
        let s = ship_label_spec();
        let zpl = render_image(&s, &noisy(100, 100), Output::Zpl).expect("zpl");
        let text = String::from_utf8(zpl).expect("ascii");
        assert!(
            text.starts_with("^XA^PW812^LL1218^LH0,0^MD30"),
            "{}",
            &text[..40]
        );
        let mut v = base();
        v.as_object_mut().unwrap().remove("zpl_darkness");
        let s = parse(&v).expect("valid");
        let zpl = render_image(&s, &noisy(100, 100), Output::Zpl).expect("zpl");
        assert!(!String::from_utf8(zpl).unwrap().contains("^MD"));
    }

    #[test]
    fn pdf_page_matches_the_label() {
        let s = ship_label_spec();
        let pdf = render_image(&s, &noisy(300, 400), Output::Pdf).expect("pdf");
        assert!(pdf.starts_with(b"%PDF-1."));
        assert!(find(&pdf, "/MediaBox [0 0 288 432]").is_some());
        assert!(find(&pdf, "/Width 812").is_some() && find(&pdf, "/Height 1218").is_some());
        assert!(find(&pdf, "/DeviceGray").is_some() && find(&pdf, "/BitsPerComponent 1").is_some());
        assert!(find(&pdf, "288 0 0 432 0 0 cm /Im0 Do").is_some());
        let at = find(&pdf, "startxref\n").expect("startxref") + "startxref\n".len();
        let n: usize = std::str::from_utf8(&pdf[at..])
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .expect("offset");
        assert!(pdf[n..].starts_with(b"xref"));
        assert!(pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn pdf_bits_are_inverted() {
        // All-black label: the 1-bit image data must be all zero bytes (PDF 0 = black).
        let s = dots((80, 8), (0, 0, 80, 8), 0, (0.0, 0.0, 0.0, 0.0));
        let pdf = render_image(&s, &png(80, 8, |_, _| BLACK), Output::Pdf).expect("pdf");
        let im = find(&pdf, "/Subtype /Image").expect("image");
        let at = im + find(&pdf[im..], "stream\n").expect("stream") + 7;
        // 80 dots = 10 bytes per row, 8 rows.
        assert!(pdf[at..at + 80].iter().all(|&b| b == 0));
    }

    // ---- text ----

    #[test]
    fn clean_text_strips_zpl_control() {
        assert_eq!(clean_text("B1^XZ^XA^PQ9999"), "B1 XZ XA PQ9999");
        assert_eq!(clean_text("  a~b\t\n c\u{7} d  "), "a b c d");
        let s = ship_label_spec();
        let zpl = render_text(&s, "B1^XZ^XA^PQ9999", Output::Zpl).expect("zpl");
        assert_eq!(count(&zpl, "^XA"), 1);
        assert_eq!(count(&zpl, "^XZ"), 1);
        assert!(find(&zpl, "^PQ").is_none());
        assert!(find(&zpl, "B1 XZ XA PQ9999").is_some());
    }

    #[test]
    fn umlauts_survive_zpl() {
        let s = ship_label_spec();
        let zpl = render_text(&s, "Müller Straße Ärger", Output::Zpl).expect("zpl");
        assert!(find(&zpl, "^CI28").is_some());
        assert!(find(&zpl, "Müller Straße Ärger").is_some());
    }

    #[test]
    fn wrap_is_greedy_and_keeps_words_whole() {
        assert_eq!(
            wrap_lines("Module Replacement needed", 12),
            ["Module", "Replacement", "needed"]
        );
        assert_eq!(wrap_lines("a b c d", 3), ["a b", "c d"]);
        assert_eq!(
            wrap_lines("Module Replacement needed", 0),
            ["Module Replacement needed"]
        );
        // A word longer than the limit stays whole instead of being cut.
        assert_eq!(
            wrap_lines("Kraftfahrzeughaftpflicht x", 5),
            ["Kraftfahrzeughaftpflicht", "x"]
        );
        assert_eq!(wrap_lines("", 12), Vec::<String>::new());
    }

    #[test]
    fn lines_per_label_chains_labels() {
        let mut v = base();
        v["text"] = json!({"wrap_chars": 1, "lines_per_label": 2});
        let s = parse(&v).expect("valid");
        let zpl = render_text(&s, "a b c", Output::Zpl).expect("zpl");
        assert_eq!(count(&zpl, "^XA"), 2);
        assert_eq!(count(&zpl, "^XZ"), 2);
        // lines_per_label 0 puts every line on one label.
        v["text"] = json!({"wrap_chars": 1, "lines_per_label": 0});
        let s = parse(&v).expect("valid");
        let zpl = render_text(&s, "a b c", Output::Zpl).expect("zpl");
        assert_eq!(count(&zpl, "^XA"), 1);
        assert_eq!(count(&zpl, "^FD"), 3);
    }

    #[test]
    fn text_pdf_has_a_page_per_label() {
        let mut v = base();
        v["text"] = json!({"wrap_chars": 1, "lines_per_label": 1});
        let s = parse(&v).expect("valid");
        let pdf = render_text(&s, "a b c", Output::Pdf).expect("pdf");
        assert_eq!(count(&pdf, "/Type /Page /"), 3);
        assert!(find(&pdf, "/Count 3").is_some());
        assert!(find(&pdf, "/BaseFont /Helvetica-Bold").is_some());
        assert!(find(&pdf, "/MediaBox [0 0 288 432]").is_some());
        assert!(find(&pdf, "WinAnsiEncoding").is_some());
        let at = find(&pdf, "startxref\n").expect("startxref") + "startxref\n".len();
        let n: usize = std::str::from_utf8(&pdf[at..])
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(pdf[n..].starts_with(b"xref"));
    }

    #[test]
    fn text_pdf_escapes_and_encodes() {
        let s = ship_label_spec();
        let pdf = render_text(&s, "Müller (x)", Output::Pdf).expect("pdf");
        assert!(
            pdf.windows(10).any(|w| w == b"(M\xfcller \\("),
            "WinAnsi byte for u-umlaut"
        );
    }

    #[test]
    fn zpl_text_never_prints_two_lines_on_one_origin() {
        // Box x=-20mm y=-20mm 60x40mm with four lines: the block hangs over the top. Lines that
        // start above the label are left out, the rest keep their own y (ZPL has no negative ^FO).
        let mut v = base();
        v["text"] = json!({"wrap_chars": 4, "lines_per_label": 0});
        v["place_mm"] = json!({"x": -20.0, "y": -20.0, "w": 60.0, "h": 40.0});
        let s = parse(&v).expect("valid");
        let zpl = render_text(&s, "AAAA BBBB CCCC DDDD", Output::Zpl).expect("zpl");
        let z = String::from_utf8(zpl).unwrap();
        let origins: Vec<&str> = z
            .split("^FO")
            .skip(1)
            .map(|p| p.split("^A0N").next().unwrap())
            .collect();
        let mut seen = origins.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), origins.len(), "duplicate origins: {origins:?}");
        assert!(
            !z.contains("AAAA"),
            "a line above the label is dropped: {z}"
        );
        assert!(z.contains("DDDD"));
        // Nothing starts at or below the bottom edge either.
        let lh = 1218;
        let mut v = base();
        v["text"] = json!({"wrap_chars": 4, "lines_per_label": 0});
        v["place_mm"] = json!({"x": 0.0, "y": 140.0, "w": 60.0, "h": 40.0});
        let s = parse(&v).expect("valid");
        let z =
            String::from_utf8(render_text(&s, "AAAA BBBB CCCC DDDD", Output::Zpl).expect("zpl"))
                .unwrap();
        for p in z.split("^FO").skip(1) {
            let y: i64 = p
                .split("^A0N")
                .next()
                .unwrap()
                .split(',')
                .nth(1)
                .unwrap()
                .parse()
                .unwrap();
            assert!(y < lh, "line at y={y} lies below the label");
        }
    }

    #[test]
    fn ascii_text_leaves_the_printer_encoding_alone() {
        // ^CI stays set on the printer and needs newer firmware, so plain text must not touch it.
        let s = ship_label_spec();
        let zpl = render_text(&s, "A-104872", Output::Zpl).expect("zpl");
        assert!(find(&zpl, "^CI28").is_none());
    }

    #[test]
    fn zpl_resets_the_label_home() {
        // ^LH stays set too: an earlier job must not shift this template.
        let s = ship_label_spec();
        let zpl = render_text(&s, "A", Output::Zpl).expect("zpl");
        assert!(find(&zpl, "^LH0,0").is_some());
    }

    #[test]
    fn the_smallest_font_keeps_zebras_minimum_width() {
        // Zebra: height and width of ^A each start at 10 dots. This box lands the font on 10.
        let mut v = base();
        v["place_mm"] = json!({"x": 0.0, "y": 0.0, "w": 60.0, "h": 1.38});
        let s = parse(&v).expect("valid");
        let zpl = render_text(&s, "AB", Output::Zpl).expect("zpl");
        assert!(find(&zpl, "^A0N,10,10").is_some());
    }

    #[test]
    fn pdf_text_budgets_for_capitals() {
        // Helvetica Bold capitals run about 0.72 em. 10 of them in 720 dots: 100 dots, 35.5 pt.
        let mut v = base();
        v["place_mm"] = json!({"x": 0.0, "y": 0.0, "w": 90.09, "h": 40.0});
        let s = parse(&v).expect("valid");
        let pdf = render_text(&s, "ABCDEFGHIK", Output::Pdf).expect("pdf");
        let text = String::from_utf8_lossy(&pdf);
        let size: f64 = text
            .split("/F1 ")
            .nth(2)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(size < 36.0, "font {size} pt runs out of its box");
    }

    #[test]
    fn a_text_block_taller_than_its_box_is_refused() {
        // 60 lines in a 30 mm box: printing would quietly lose most of them.
        let mut v = base();
        v["text"] = json!({"wrap_chars": 6, "lines_per_label": 0});
        v["place_mm"] = json!({"x": 0.0, "y": 0.0, "w": 40.0, "h": 30.0});
        let s = parse(&v).expect("valid");
        let text = vec!["abcde"; 60].join(" ");
        for out in [Output::Zpl, Output::Pdf] {
            let e = render_text(&s, &text, out).expect_err("too much text");
            assert!(e.contains("does not fit"), "{e}");
        }
    }

    #[test]
    fn text_font_shrinks_to_fit() {
        let s = ship_label_spec();
        let size = |t: &str| {
            let zpl = render_text(&s, t, Output::Zpl).expect("zpl");
            let z = String::from_utf8(zpl).unwrap();
            let at = z.find("^A0N,").unwrap() + 5;
            z[at..].split(',').next().unwrap().parse::<u32>().unwrap()
        };
        assert!(size("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789") < size("AB"));
        assert!((10..=400).contains(&size("AB")));
    }

    #[test]
    fn empty_text_is_refused() {
        let s = ship_label_spec();
        assert_eq!(
            render_text(&s, "", Output::Zpl),
            Err("text is empty".into())
        );
        assert!(render_text(&s, " ^~ ", Output::Pdf).is_err());
    }

    #[test]
    fn too_long_text_is_refused() {
        let s = ship_label_spec();
        let at_limit = "a ".repeat(MAX_TEXT / 2);
        let long = "a".repeat(MAX_TEXT + 1);
        let e = render_text(&s, &long, Output::Zpl).expect_err("over the limit");
        assert!(e.contains("longer than"), "{e}");
        let fits = render_text(&s, &at_limit, Output::Zpl);
        assert!(!fits.is_err_and(|e| e.contains("longer than")));
    }
}

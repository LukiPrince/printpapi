// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Receipts from JSON: one structured document is laid out once, on a grid of 12 x 24 dot cells,
//! and written as ESC/POS bytes, as a PDF or as a preview image. Pure: values in, bytes out, no
//! IO, no clock. The agent never sees a receipt, only the `raw` or `pdf` bytes made here.
//!
//! Only these commands are written: `ESC @`, `ESC R 0`, `ESC t n`, `ESC E n`, `GS ! n`, `LF`,
//! `GS v 0`, `ESC d n`, `GS V 65/66 0` and `ESC p m 25 250`. Sources, read 2026-11-24:
//! - Epson ESC/POS reference (GS v 0 raster, ESC @, GS V; the other commands are on the same
//!   site under the same names): <https://web.archive.org/web/2024id_/https://download4.epson.biz/sec_pubs/pos/reference_en/escpos/gs_lv_0.html>,
//!   <https://web.archive.org/web/2024id_/https://download4.epson.biz/sec_pubs/pos/reference_en/escpos/esc_atsign.html>,
//!   <https://web.archive.org/web/2024id_/https://download4.epson.biz/sec_pubs/pos/reference_en/escpos/gs_cv.html>
//!   (`GS V 65` is the full cut, `GS V 66` the partial cut, both with n = 0).
//! - CP1252: <https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/WINDOWS/CP1252.TXT>
//! - Code pages per printer (a check only, nothing copied):
//!   <https://raw.githubusercontent.com/receipt-print-hq/escpos-printer-db/master/dist/capabilities.json>
//! - QR: <https://docs.rs/qrcodegen/1.8.0/qrcodegen/struct.QrCode.html>; Code 128 and EAN-13:
//!   <https://docs.rs/crate/barcoders/2.0.0/source/>
//! - Terminus Font 4.49.1 (SIL OFL 1.1), see `font`.
//!
//! ESC/POS is a trademark of Seiko Epson Corporation; this project is not affiliated with Epson.

pub mod codes;
pub mod escpos;
pub mod font;
pub mod layout;
pub mod raster;
pub mod text;

use base64::Engine;
use serde_json::{Map, Value, json};

pub use escpos::escpos;
pub use layout::{Layout, Op, layout};
pub use raster::{pdf, preview_png};

/// Dots per character cell, and per line of text at size 1 (font A).
pub const CELL_W: u32 = font::CELL_W;
pub const CELL_H: u32 = font::CELL_H;
/// What a line of text takes at size `h`: the power-on line spacing, never less than the glyph.
pub fn pitch(h: u8) -> u32 {
    (CELL_H * u32::from(h)).max(30)
}

// --- limits -----------------------------------------------------------------------------------

pub const MAX_ELEMENTS: usize = 500;
pub const MAX_TEXT_CHARS: usize = 20_000;
pub const MAX_ELEMENT_TEXT: usize = 4000;
pub const MAX_IMAGES: usize = 8;
pub const MAX_CODES: usize = 20;
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
pub const MAX_IMAGE_BYTES_TOTAL: usize = 2 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 4_000_000;
pub const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;
/// 2.5 m of paper at 8 dots per millimetre.
pub const MAX_ROWS: u32 = 20_000;
pub const MAX_QR_BYTES: usize = 2000;
/// Characters of a divider's `char` before cleaning (one printable character is what is left).
pub const MAX_DIVIDER_RAW: usize = 16;

// --- profile ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codepage {
    /// Every text row is drawn with the embedded font.
    Raster,
    /// Rows whose characters all encode go out as native text.
    Cp1252,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutMode {
    Partial,
    Full,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// PDF on a printer that takes PDF, ESC/POS otherwise.
    Auto,
    /// Always ESC/POS.
    Escpos,
}

/// How one printer wants its receipts. Stored per printer by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub paper_mm: u32,
    pub width_dots: u32,
    pub codepage: Codepage,
    pub codepage_number: u8,
    pub cut: CutMode,
    pub output: OutputMode,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            paper_mm: 80,
            width_dots: 576,
            codepage: Codepage::Raster,
            codepage_number: 16,
            cut: CutMode::Partial,
            output: OutputMode::Auto,
        }
    }
}

impl Profile {
    /// Characters per line at size 1.
    pub fn cols(&self) -> u32 {
        self.width_dots / CELL_W
    }
}

const PROFILE_KEYS: [&str; 6] = [
    "paper_mm",
    "width_dots",
    "codepage",
    "codepage_number",
    "cut",
    "output",
];

/// The profile of the contract JSON. Omitted fields take the defaults (the width follows the
/// paper); unknown fields and out-of-range values are an error.
pub fn parse_profile(v: &Map<String, Value>) -> Result<Profile, String> {
    only_keys(v, &PROFILE_KEYS, "profile")?;
    let mut p = Profile::default();
    if let Some(x) = field(v, "paper_mm") {
        p.paper_mm = match x.as_i64() {
            Some(58) => 58,
            Some(80) => 80,
            _ => return Err("paper_mm must be 58 or 80".into()),
        };
        p.width_dots = if p.paper_mm == 58 { 384 } else { 576 };
    }
    if let Some(x) = field(v, "width_dots") {
        // the most is what the paper takes at 8 dots a millimetre; the default stays the usual
        // print width of each paper (384, 576)
        let max = i64::from(p.paper_mm) * 8;
        p.width_dots = match x.as_i64() {
            Some(n) if n % 8 == 0 && (192..=max).contains(&n) => n as u32,
            _ => {
                return Err(format!(
                    "width_dots must be a multiple of 8 from 192 to {max}"
                ));
            }
        };
    }
    if let Some(x) = field(v, "codepage") {
        p.codepage = match x.as_str() {
            Some("raster") => Codepage::Raster,
            Some("cp1252") => Codepage::Cp1252,
            _ => return Err("codepage must be raster or cp1252".into()),
        };
    }
    if let Some(x) = field(v, "codepage_number") {
        p.codepage_number = match x.as_i64() {
            Some(n) if (0..=255).contains(&n) => n as u8,
            _ => return Err("codepage_number must be a whole number from 0 to 255".into()),
        };
    }
    if let Some(x) = field(v, "cut") {
        p.cut = match x.as_str() {
            Some("partial") => CutMode::Partial,
            Some("full") => CutMode::Full,
            Some("none") => CutMode::None,
            _ => return Err("cut must be partial, full or none".into()),
        };
    }
    if let Some(x) = field(v, "output") {
        p.output = match x.as_str() {
            Some("auto") => OutputMode::Auto,
            Some("escpos") => OutputMode::Escpos,
            _ => return Err("output must be auto or escpos".into()),
        };
    }
    Ok(p)
}

/// `null` reads as omitted.
fn field<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    m.get(key).filter(|v| !v.is_null())
}

pub fn profile_json(p: &Profile) -> Value {
    json!({
        "paper_mm": p.paper_mm,
        "width_dots": p.width_dots,
        "codepage": match p.codepage { Codepage::Raster => "raster", Codepage::Cp1252 => "cp1252" },
        "codepage_number": p.codepage_number,
        "cut": match p.cut { CutMode::Partial => "partial", CutMode::Full => "full", CutMode::None => "none" },
        "output": match p.output { OutputMode::Auto => "auto", OutputMode::Escpos => "escpos" },
    })
}

// --- document ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    pub align: Align,
    pub bold: bool,
    pub width: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ec {
    L,
    M,
    Q,
    H,
}

impl Ec {
    pub fn letter(self) -> char {
        match self {
            Ec::L => 'L',
            Ec::M => 'M',
            Ec::Q => 'Q',
            Ec::H => 'H',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarFormat {
    Code128,
    Ean13,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Element {
    Text {
        text: String,
        align: Align,
        bold: bool,
        size: (u8, u8),
    },
    Columns {
        cells: Vec<Cell>,
        size: (u8, u8),
    },
    Divider {
        ch: char,
    },
    Image {
        bytes: Vec<u8>,
        align: Align,
        width_percent: Option<u32>,
        dither: bool,
    },
    Qr {
        data: String,
        ec: Ec,
        size: Option<u32>,
        align: Align,
    },
    Barcode {
        format: BarFormat,
        data: String,
        height: u32,
        size: Option<u32>,
        text: bool,
        align: Align,
    },
    Feed(u8),
    Cut,
    Drawer(u8),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub content: Vec<Element>,
    pub cut: bool,
}

impl Doc {
    /// Whether any element decodes an image (what needs a render slot).
    pub fn has_images(&self) -> bool {
        self.content
            .iter()
            .any(|e| matches!(e, Element::Image { .. }))
    }
}

fn only_keys(m: &Map<String, Value>, allowed: &[&str], what: &str) -> Result<(), String> {
    match m.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown field in {what}: {k}")),
        None => Ok(()),
    }
}

fn get_bool(m: &Map<String, Value>, key: &str, path: &str, default: bool) -> Result<bool, String> {
    match field(m, key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{path}.{key} must be true or false")),
    }
}

fn get_align(m: &Map<String, Value>, path: &str, default: Align) -> Result<Align, String> {
    match field(m, "align") {
        None => Ok(default),
        Some(v) => match v.as_str() {
            Some("left") => Ok(Align::Left),
            Some("center") => Ok(Align::Center),
            Some("right") => Ok(Align::Right),
            _ => Err(format!("{path}.align must be left, center or right")),
        },
    }
}

fn get_size(m: &Map<String, Value>, path: &str) -> Result<(u8, u8), String> {
    let bad = || format!("{path}.size must be 1-8 or [w, h]");
    let one = |v: &Value| match v.as_i64() {
        Some(n) if (1..=8).contains(&n) => Ok(n as u8),
        _ => Err(bad()),
    };
    match field(m, "size") {
        None => Ok((1, 1)),
        Some(Value::Array(a)) if a.len() == 2 => Ok((one(&a[0])?, one(&a[1])?)),
        Some(Value::Array(_)) => Err(bad()),
        Some(v) => one(v).map(|n| (n, n)),
    }
}

/// An optional whole number in `lo..=hi`.
fn get_int(
    m: &Map<String, Value>,
    key: &str,
    path: &str,
    lo: i64,
    hi: i64,
) -> Result<Option<i64>, String> {
    match field(m, key) {
        None => Ok(None),
        Some(v) => match v.as_i64() {
            Some(n) if (lo..=hi).contains(&n) => Ok(Some(n)),
            _ => Err(format!(
                "{path}.{key} must be a whole number from {lo} to {hi}"
            )),
        },
    }
}

fn get_text(m: &Map<String, Value>, key: &str, path: &str, min: usize) -> Result<String, String> {
    match field(m, key) {
        None => Err(format!("{path}.{key} is required")),
        Some(Value::String(s)) => {
            let n = s.chars().count();
            if n < min || n > MAX_ELEMENT_TEXT {
                return Err(format!(
                    "{path}.{key} must be {min} to {MAX_ELEMENT_TEXT} characters"
                ));
            }
            Ok(s.clone())
        }
        Some(_) => Err(format!("{path}.{key} must be a string")),
    }
}

/// What parsing has used up so far (the limits that need no decoding).
#[derive(Default)]
struct Budget {
    text_chars: usize,
    images: usize,
    image_bytes: usize,
    codes: usize,
}

/// Parse and validate the `receipt` object of a job. No image is decoded here (the file size and
/// the base64 are checked); every error names its path, `receipt.content[3].size ...`.
pub fn parse_doc(v: &Value) -> Result<Doc, String> {
    let m = v.as_object().ok_or("receipt must be an object")?;
    only_keys(m, &["content", "cut"], "receipt")?;
    let cut = get_bool(m, "cut", "receipt", true)?;
    let items = match m.get("content") {
        None | Some(Value::Null) => return Err("receipt.content is required".into()),
        Some(Value::Array(a)) if (1..=MAX_ELEMENTS).contains(&a.len()) => a,
        Some(_) => {
            return Err(format!(
                "receipt.content must be a list of 1 to {MAX_ELEMENTS} elements"
            ));
        }
    };
    let mut budget = Budget::default();
    let mut content = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        content.push(parse_element(item, i, &mut budget)?);
    }
    Ok(Doc { content, cut })
}

fn count_text(b: &mut Budget, n: usize) -> Result<(), String> {
    b.text_chars += n;
    if b.text_chars > MAX_TEXT_CHARS {
        return Err(format!(
            "receipt has more than {MAX_TEXT_CHARS} characters of text"
        ));
    }
    Ok(())
}

fn parse_element(v: &Value, i: usize, b: &mut Budget) -> Result<Element, String> {
    let path = format!("receipt.content[{i}]");
    let m = v
        .as_object()
        .ok_or_else(|| format!("{path} must be an object"))?;
    let kind = match m.get("type") {
        Some(Value::String(s)) => s.as_str(),
        None | Some(Value::Null) => return Err(format!("{path}.type is required")),
        Some(_) => return Err(format!("{path}.type must be a string")),
    };
    match kind {
        "text" => {
            only_keys(m, &["type", "text", "align", "bold", "size"], &path)?;
            let raw = get_text(m, "text", &path, 1)?;
            let text = text::normalize(&raw);
            count_text(b, text.chars().count())?;
            Ok(Element::Text {
                text,
                align: get_align(m, &path, Align::Left)?,
                bold: get_bool(m, "bold", &path, false)?,
                size: get_size(m, &path)?,
            })
        }
        "columns" => {
            only_keys(m, &["type", "cells", "size"], &path)?;
            let size = get_size(m, &path)?;
            let cells = match m.get("cells") {
                Some(Value::Array(a)) if (2..=6).contains(&a.len()) => a,
                None | Some(Value::Null) => return Err(format!("{path}.cells is required")),
                Some(_) => return Err(format!("{path}.cells must be a list of 2 to 6 cells")),
            };
            let mut out = Vec::with_capacity(cells.len());
            for (j, c) in cells.iter().enumerate() {
                let cp = format!("{path}.cells[{j}]");
                let cm = c
                    .as_object()
                    .ok_or_else(|| format!("{cp} must be an object"))?;
                only_keys(cm, &["text", "align", "bold", "width"], &cp)?;
                let text = text::normalize(&get_text(cm, "text", &cp, 0)?);
                count_text(b, text.chars().count())?;
                out.push(Cell {
                    text,
                    align: get_align(cm, &cp, Align::Left)?,
                    bold: get_bool(cm, "bold", &cp, false)?,
                    width: get_int(cm, "width", &cp, 1, 100)?.map(|n| n as u32),
                });
            }
            Ok(Element::Columns { cells: out, size })
        }
        "divider" => {
            only_keys(m, &["type", "char"], &path)?;
            let ch = match field(m, "char") {
                None => '-',
                // a handful of characters is room enough for one base character and its
                // marks; anything longer is refused before it is cleaned (a body can be 32 MiB)
                Some(Value::String(s)) if s.chars().nth(MAX_DIVIDER_RAW).is_some() => {
                    return Err(format!("{path}.char must be one printable character"));
                }
                Some(Value::String(s)) => {
                    let n = text::normalize(s);
                    let mut it = n.chars();
                    match (it.next(), it.next()) {
                        (Some(c), None) if !c.is_whitespace() => c,
                        _ => return Err(format!("{path}.char must be one printable character")),
                    }
                }
                Some(_) => return Err(format!("{path}.char must be one printable character")),
            };
            Ok(Element::Divider { ch })
        }
        "image" => {
            only_keys(
                m,
                &["type", "content", "align", "width_percent", "dither"],
                &path,
            )?;
            b.images += 1;
            if b.images > MAX_IMAGES {
                return Err(format!("receipt has more than {MAX_IMAGES} images"));
            }
            let Some(Value::String(s)) = field(m, "content") else {
                return Err(format!("{path}.content must be base64 image data"));
            };
            // the decoded size is known from the length, before anything is allocated
            if s.len() / 4 * 3 > MAX_IMAGE_BYTES + 3 {
                return Err(format!(
                    "{path}: image is larger than {MAX_IMAGE_BYTES} bytes"
                ));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(|_| format!("{path}.content is not valid base64"))?;
            if bytes.is_empty() {
                return Err(format!("{path}.content must be base64 image data"));
            }
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(format!(
                    "{path}: image is larger than {MAX_IMAGE_BYTES} bytes"
                ));
            }
            b.image_bytes += bytes.len();
            if b.image_bytes > MAX_IMAGE_BYTES_TOTAL {
                return Err(format!(
                    "receipt has more than {MAX_IMAGE_BYTES_TOTAL} bytes of images"
                ));
            }
            Ok(Element::Image {
                bytes,
                align: get_align(m, &path, Align::Center)?,
                width_percent: get_int(m, "width_percent", &path, 1, 100)?.map(|n| n as u32),
                dither: get_bool(m, "dither", &path, false)?,
            })
        }
        "qr" => {
            only_keys(m, &["type", "data", "ec", "size", "align"], &path)?;
            b.codes += 1;
            if b.codes > MAX_CODES {
                return Err(format!(
                    "receipt has more than {MAX_CODES} QR codes and barcodes"
                ));
            }
            let data = match field(m, "data") {
                Some(Value::String(s)) if !s.is_empty() && s.len() <= MAX_QR_BYTES => s.clone(),
                None => return Err(format!("{path}.data is required")),
                _ => {
                    return Err(format!(
                        "{path}.data must be a string of 1 to {MAX_QR_BYTES} bytes"
                    ));
                }
            };
            let ec = match field(m, "ec") {
                None => Ec::M,
                Some(v) => match v.as_str() {
                    Some("L") => Ec::L,
                    Some("M") => Ec::M,
                    Some("Q") => Ec::Q,
                    Some("H") => Ec::H,
                    _ => return Err(format!("{path}.ec must be L, M, Q or H")),
                },
            };
            Ok(Element::Qr {
                data,
                ec,
                size: get_int(m, "size", &path, 1, 16)?.map(|n| n as u32),
                align: get_align(m, &path, Align::Center)?,
            })
        }
        "barcode" => {
            only_keys(
                m,
                &["type", "format", "data", "height", "size", "text", "align"],
                &path,
            )?;
            b.codes += 1;
            if b.codes > MAX_CODES {
                return Err(format!(
                    "receipt has more than {MAX_CODES} QR codes and barcodes"
                ));
            }
            let format = match m.get("format").and_then(Value::as_str) {
                Some("code128") => BarFormat::Code128,
                Some("ean13") => BarFormat::Ean13,
                _ => return Err(format!("{path}.format must be code128 or ean13")),
            };
            let data = match field(m, "data") {
                Some(Value::String(s)) => s.clone(),
                None => return Err(format!("{path}.data is required")),
                Some(_) => return Err(format!("{path}.data must be a string")),
            };
            codes::check_barcode(format, &data).map_err(|e| format!("{path}{e}"))?;
            count_text(b, data.chars().count())?;
            Ok(Element::Barcode {
                format,
                data,
                height: get_int(m, "height", &path, 24, 240)?.map_or(80, |n| n as u32),
                size: get_int(m, "size", &path, 1, 6)?.map(|n| n as u32),
                text: get_bool(m, "text", &path, true)?,
                align: get_align(m, &path, Align::Center)?,
            })
        }
        "feed" => {
            only_keys(m, &["type", "lines"], &path)?;
            Ok(Element::Feed(
                get_int(m, "lines", &path, 1, 20)?.map_or(1, |n| n as u8),
            ))
        }
        "cut" => {
            only_keys(m, &["type"], &path)?;
            Ok(Element::Cut)
        }
        "drawer" => {
            only_keys(m, &["type", "pin"], &path)?;
            let pin = match field(m, "pin") {
                None => 2,
                Some(v) => match v.as_i64() {
                    Some(2) => 2,
                    Some(5) => 5,
                    _ => return Err(format!("{path}.pin must be 2 or 5")),
                },
            };
            Ok(Element::Drawer(pin))
        }
        other => Err(format!("{path}: unknown type {other:?}")),
    }
}

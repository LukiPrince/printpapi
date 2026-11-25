// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Receipts, pure: parsing, layout, the ESC/POS bytes, the PDF and the preview, with golden files
//! (ESC/POS as hex text, bitmaps as PNG compared by decoded pixels: the encoder's bytes are not
//! stable). Run with UPDATE_GOLDEN=1 to rewrite the goldens, and the demo's sample images with them.
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use image::{GrayImage, ImageEncoder, Luma, codecs::png::PngEncoder};
use printpapi_server::receipt::{
    self, Codepage, CutMode, Doc, Layout, Op, OutputMode, Profile, escpos, layout, parse_doc,
    parse_profile, pdf, preview_png, profile_json,
};
use serde_json::{Value, json};
use std::path::PathBuf;

fn doc(v: Value) -> Doc {
    parse_doc(&v).unwrap_or_else(|e| panic!("{e}"))
}

fn prof(v: Value) -> Profile {
    parse_profile(v.as_object().unwrap()).unwrap_or_else(|e| panic!("{e}"))
}

fn cp1252() -> Profile {
    prof(json!({"codepage": "cp1252"}))
}

fn lay(content: Value, p: &Profile) -> Layout {
    layout(&doc(json!({ "content": content })), p).unwrap_or_else(|e| panic!("{e}"))
}

fn lay_err(content: Value, p: &Profile) -> String {
    layout(&doc(json!({ "content": content })), p).unwrap_err()
}

fn stream(content: Value, p: &Profile) -> Vec<u8> {
    escpos(&lay(content, p), p)
}

/// `n` copies of `v` as a JSON array.
fn rep(v: Value, n: usize) -> Value {
    Value::Array(vec![v; n])
}

fn err(v: Value) -> String {
    parse_doc(&v).unwrap_err()
}

fn text_rows(l: &Layout) -> Vec<String> {
    l.ops
        .iter()
        .filter_map(|o| match o {
            Op::Row { runs, .. } => Some(runs.iter().map(|r| r.0.as_str()).collect::<String>()),
            _ => None,
        })
        .collect()
}

fn kinds(l: &Layout) -> Vec<&'static str> {
    l.ops
        .iter()
        .map(|o| match o {
            Op::Row { .. } => "row",
            Op::Image { .. } => "image",
            Op::Feed(_) => "feed",
            Op::Cut => "cut",
            Op::Drawer(_) => "drawer",
        })
        .collect()
}

fn png_b64(w: u32, h: u32, shade: u8) -> String {
    let img = GrayImage::from_pixel(w, h, Luma([shade]));
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::L8)
        .unwrap();
    B64.encode(out)
}

/// The GS v 0 bitmaps of a stream: (bytes per row, rows, data), and the bytes outside them.
type Raster = (usize, usize, Vec<u8>);

fn scan(s: &[u8]) -> (Vec<Raster>, Vec<u8>) {
    let (mut maps, mut rest, mut i) = (Vec::new(), Vec::new(), 0);
    while i < s.len() {
        let skip = match (s[i], s.get(i + 1).copied()) {
            (0x1d, Some(0x76)) => {
                let (xb, yr) = (
                    usize::from(s[i + 4]) | usize::from(s[i + 5]) << 8,
                    usize::from(s[i + 6]) | usize::from(s[i + 7]) << 8,
                );
                maps.push((xb, yr, s[i + 8..i + 8 + xb * yr].to_vec()));
                i += 8 + xb * yr;
                continue;
            }
            (0x1d, Some(0x56)) => 4,
            (0x1d, Some(0x21)) => 3,
            (0x1b, Some(0x40)) => 2,
            (0x1b, Some(0x52 | 0x74 | 0x45 | 0x64)) => 3,
            (0x1b, Some(0x70)) => 5,
            _ => 1,
        };
        rest.extend_from_slice(&s[i..i + skip]);
        i += skip;
    }
    (maps, rest)
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn unhex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|h| u8::from_str_radix(h, 16).unwrap())
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/receipts")
        .join(name)
}

fn updating() -> bool {
    std::env::var_os("UPDATE_GOLDEN").is_some()
}

/// Hex goldens: 32 bytes a line, LF line ends.
fn golden_hex(name: &str, bytes: &[u8]) {
    let text: String = bytes.chunks(32).map(|c| hex(c) + "\n").collect();
    let path = fixture(name);
    if updating() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("missing {name}"));
    assert_eq!(
        unhex(&want),
        bytes,
        "golden {name} differs (UPDATE_GOLDEN=1 rewrites it)"
    );
}

fn decode_png(b: &[u8]) -> GrayImage {
    image::load_from_memory(b).unwrap().to_luma8()
}

fn sample() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/lib/receipt-sample.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn demo_asset(paper: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../web/demo/static/receipt-sample-{paper}.png"))
}

fn profile_of(paper: u32, codepage: &str) -> Profile {
    prof(json!({"paper_mm": paper, "codepage": codepage}))
}

// --- 6 profile ----------------------------------------------------------------------------------

#[test]
fn profile_defaults_and_round_trip() {
    let p = prof(json!({}));
    assert_eq!(p, Profile::default());
    assert_eq!((p.paper_mm, p.width_dots, p.cols()), (80, 576, 48));
    assert_eq!(p.codepage, Codepage::Raster);
    assert_eq!(prof(json!({"paper_mm": 58})).width_dots, 384);
    let full = json!({"paper_mm": 58, "width_dots": 320, "codepage": "cp1252", "codepage_number": 19,
                      "cut": "full", "output": "escpos"});
    let back = profile_json(&prof(full.clone()));
    assert_eq!(back, full);
    assert_eq!(prof(back).cut, CutMode::Full);
    assert_eq!(prof(json!({"output": "escpos"})).output, OutputMode::Escpos);
    // null reads as omitted
    assert_eq!(prof(json!({"cut": null})), Profile::default());
}

#[test]
fn profile_ranges_and_unknowns() {
    let bad = |v: Value| parse_profile(v.as_object().unwrap()).unwrap_err();
    for w in [192, 200, 384, 576, 640] {
        assert!(
            parse_profile(json!({"width_dots": w}).as_object().unwrap()).is_ok(),
            "{w}"
        );
    }
    // the most is paper_mm * 8: 58 mm heads of 416, 432 and 464 dots exist, so do 80 mm ones of 640
    for w in [416, 432, 464] {
        let p = json!({"paper_mm": 58, "width_dots": w});
        assert_eq!(parse_profile(p.as_object().unwrap()).unwrap().width_dots, w);
    }
    assert!(bad(json!({"width_dots": 648})).contains("192 to 640"));
    assert!(bad(json!({"paper_mm": 58, "width_dots": 472})).contains("192 to 464"));
    assert!(bad(json!({"width_dots": 184})).starts_with("width_dots must be"));
    assert!(bad(json!({"width_dots": 580})).starts_with("width_dots must be"));
    assert!(bad(json!({"paper_mm": 58, "width_dots": 468})).contains("192 to 464"));
    assert!(bad(json!({"paper_mm": 57})).contains("58 or 80"));
    assert!(bad(json!({"codepage": "cp850"})).contains("raster or cp1252"));
    assert!(bad(json!({"codepage_number": 256})).contains("0 to 255"));
    assert!(bad(json!({"codepage_number": -1})).contains("0 to 255"));
    assert!(parse_profile(json!({"codepage_number": 0}).as_object().unwrap()).is_ok());
    assert!(parse_profile(json!({"codepage_number": 255}).as_object().unwrap()).is_ok());
    assert!(bad(json!({"cut": "half"})).contains("partial, full or none"));
    assert!(bad(json!({"output": "pdf"})).contains("auto or escpos"));
    assert_eq!(
        bad(json!({"colour": 1})),
        "unknown field in profile: colour"
    );
}

// --- 7 document ---------------------------------------------------------------------------------

#[test]
fn every_element_has_its_defaults() {
    let d = doc(json!({"content": [
        {"type": "text", "text": "x"}, {"type": "columns", "cells": [{"text": "a"}, {"text": "b"}]},
        {"type": "divider"}, {"type": "qr", "data": "d"},
        {"type": "barcode", "format": "code128", "data": "d"}, {"type": "feed"},
        {"type": "cut"}, {"type": "drawer"}]}));
    assert!(d.cut);
    use receipt::{Align, BarFormat, Ec, Element};
    assert_eq!(
        d.content[0],
        Element::Text {
            text: "x".into(),
            align: Align::Left,
            bold: false,
            size: (1, 1)
        }
    );
    assert_eq!(d.content[2], Element::Divider { ch: '-' });
    assert_eq!(
        d.content[3],
        Element::Qr {
            data: "d".into(),
            ec: Ec::M,
            size: None,
            align: Align::Center
        }
    );
    assert_eq!(
        d.content[4],
        Element::Barcode {
            format: BarFormat::Code128,
            data: "d".into(),
            height: 80,
            size: None,
            text: true,
            align: Align::Center
        }
    );
    assert_eq!(d.content[5], Element::Feed(1));
    assert_eq!(d.content[7], Element::Drawer(2));
    assert!(!d.has_images());
    let off = doc(json!({"cut": false, "content": [{"type": "cut"}]}));
    assert!(!off.cut);
}

#[test]
fn every_range_edge() {
    let ok = |e: Value| parse_doc(&json!({"content": [e]})).map(|_| ());
    for e in [
        json!({"type": "text", "text": "x", "size": 8}),
        json!({"type": "text", "text": "x", "size": [8, 1]}),
        json!({"type": "feed", "lines": 20}),
        json!({"type": "feed", "lines": 1}),
        json!({"type": "drawer", "pin": 5}),
        json!({"type": "qr", "data": "d", "size": 16}),
        json!({"type": "qr", "data": "d", "size": 1}),
        json!({"type": "barcode", "format": "ean13", "data": "400638133393", "height": 240, "size": 6}),
        json!({"type": "barcode", "format": "code128", "data": "d", "height": 24, "size": 1}),
    ] {
        assert_eq!(ok(e.clone()), Ok(()), "{e}");
    }
    for (e, want) in [
        (
            json!({"type": "feed", "lines": 0}),
            "receipt.content[0].lines must be a whole number from 1 to 20",
        ),
        (
            json!({"type": "feed", "lines": 21}),
            "receipt.content[0].lines must be a whole number from 1 to 20",
        ),
        (
            json!({"type": "drawer", "pin": 3}),
            "receipt.content[0].pin must be 2 or 5",
        ),
        (
            json!({"type": "qr", "data": "d", "size": 17}),
            "receipt.content[0].size must be a whole number from 1 to 16",
        ),
        (
            json!({"type": "qr", "data": "d", "ec": "X"}),
            "receipt.content[0].ec must be L, M, Q or H",
        ),
        (
            json!({"type": "qr", "data": ""}),
            "receipt.content[0].data must be a string of 1 to 2000 bytes",
        ),
        (
            json!({"type": "qr", "data": "a".repeat(2001)}),
            "receipt.content[0].data must be a string of 1 to 2000 bytes",
        ),
        (
            json!({"type": "barcode", "format": "code128", "data": "d", "height": 23}),
            "receipt.content[0].height must be a whole number from 24 to 240",
        ),
        (
            json!({"type": "barcode", "format": "code128", "data": "d", "size": 7}),
            "receipt.content[0].size must be a whole number from 1 to 6",
        ),
        (
            json!({"type": "barcode", "format": "upca", "data": "d"}),
            "receipt.content[0].format must be code128 or ean13",
        ),
        (
            json!({"type": "text", "text": ""}),
            "receipt.content[0].text must be 1 to 4000 characters",
        ),
        (
            json!({"type": "text", "text": "a".repeat(4001)}),
            "receipt.content[0].text must be 1 to 4000 characters",
        ),
        (
            json!({"type": "text"}),
            "receipt.content[0].text is required",
        ),
        (
            json!({"type": "text", "text": "x", "align": "middle"}),
            "receipt.content[0].align must be left, center or right",
        ),
        (
            json!({"type": "text", "text": "x", "bold": "yes"}),
            "receipt.content[0].bold must be true or false",
        ),
        (
            json!({"type": "columns", "cells": [{"text": "a"}]}),
            "receipt.content[0].cells must be a list of 2 to 6 cells",
        ),
        (
            json!({"type": "columns", "cells": rep(json!({"text": "a"}), 7)}),
            "receipt.content[0].cells must be a list of 2 to 6 cells",
        ),
        (
            json!({"type": "columns", "cells": [{"text": "a"}, {"text": "b", "width": 0}]}),
            "receipt.content[0].cells[1].width must be a whole number from 1 to 100",
        ),
        (
            json!({"type": "image", "content": "QUJD", "width_percent": 0}),
            "receipt.content[0].width_percent must be a whole number from 1 to 100",
        ),
        (
            json!({"type": "image", "content": "QUJD", "width_percent": 101}),
            "receipt.content[0].width_percent must be a whole number from 1 to 100",
        ),
    ] {
        assert_eq!(ok(e.clone()).unwrap_err(), want, "{e}");
    }
    assert!(
        parse_doc(
            &json!({"content": [{"type": "columns", "cells": rep(json!({"text": "a"}), 6)}]})
        )
        .is_ok()
    );
}

#[test]
fn unknown_things_name_their_path() {
    assert_eq!(
        err(json!({"content": [{"type": "text", "text": "x"}, {"type": "foo"}]})),
        "receipt.content[1]: unknown type \"foo\""
    );
    assert_eq!(
        err(json!({"content": [{"type": "text", "text": "x", "foo": 1}]})),
        "unknown field in receipt.content[0]: foo"
    );
    assert_eq!(
        err(
            json!({"content": [{"type": "columns", "cells": [{"text": "a"}, {"text": "b", "x": 1}]}]})
        ),
        "unknown field in receipt.content[0].cells[1]: x"
    );
    assert_eq!(
        err(json!({"content": [{"text": "x"}]})),
        "receipt.content[0].type is required"
    );
    assert_eq!(
        err(json!({"content": ["x"]})),
        "receipt.content[0] must be an object"
    );
    assert_eq!(
        err(json!({"content": [{"type": "text", "text": "x"}], "foo": 1})),
        "unknown field in receipt: foo"
    );
    assert_eq!(err(json!({})), "receipt.content is required");
    assert_eq!(err(json!([])), "receipt must be an object");
    assert_eq!(
        err(json!({"content": [{"type": "text", "text": "x", "size": 9}]})),
        "receipt.content[0].size must be 1-8 or [w, h]"
    );
    assert_eq!(
        err(json!({"content": [{"type": "barcode", "format": "ean13", "data": "4006381333930"}]})),
        "receipt.content[0]: ean13 check digit is wrong"
    );
}

#[test]
fn the_divider_character_is_checked_after_normalisation() {
    let d = |c: &str| parse_doc(&json!({"content": [{"type": "divider", "char": c}]}));
    for ok in ["=", "*", "€", "e\u{301}", "ü"] {
        assert!(d(ok).is_ok(), "{ok:?}");
    }
    // one character plus a few marks is fine; a long string is refused before it is cleaned
    assert!(d(&format!("={}", "\u{200b}".repeat(8))).is_ok());
    assert!(d(&format!("={}", "\u{200b}".repeat(40))).is_err());
    assert!(d(&"\u{301}".repeat(100_000)).is_err());
    for bad in [
        "\u{ad}", "\u{200b}", "\u{202d}", "\u{202f}", "\u{2060}", "\u{fe0f}", "\u{301}",
        "\u{1e9e}", " ", "\n", "\u{1b}", "ab", "", "\t", "\u{a0}",
    ] {
        assert_eq!(
            d(bad).unwrap_err(),
            "receipt.content[0].char must be one printable character",
            "{bad:?}"
        );
    }
}

// --- 8 limits -----------------------------------------------------------------------------------

#[test]
fn document_limits() {
    let text = json!({"type": "text", "text": "x"});
    assert_eq!(
        err(json!({"content": []})),
        "receipt.content must be a list of 1 to 500 elements"
    );
    assert!(parse_doc(&json!({"content": vec![text.clone(); 500]})).is_ok());
    assert!(err(json!({"content": vec![text.clone(); 501]})).contains("1 to 500 elements"));
    // 5 x 4000 characters is the limit, one more is not
    let big = json!({"type": "text", "text": "a".repeat(4000)});
    let mut c = vec![big; 5];
    assert!(parse_doc(&json!({"content": c.clone()})).is_ok());
    c.push(text.clone());
    assert_eq!(
        err(json!({"content": c})),
        "receipt has more than 20000 characters of text"
    );
    let img = json!({"type": "image", "content": png_b64(4, 4, 0)});
    assert!(parse_doc(&json!({"content": vec![img.clone(); 8]})).is_ok());
    assert_eq!(
        err(json!({"content": vec![img.clone(); 9]})),
        "receipt has more than 8 images"
    );
    let qr = json!({"type": "qr", "data": "x"});
    let bar = json!({"type": "barcode", "format": "code128", "data": "x"});
    let mut codes = vec![qr.clone(); 10];
    codes.extend(vec![bar.clone(); 10]);
    assert!(parse_doc(&json!({"content": codes.clone()})).is_ok());
    codes.push(qr);
    assert_eq!(
        err(json!({"content": codes})),
        "receipt has more than 20 QR codes and barcodes"
    );
    // a file over 1 MiB
    let huge = B64.encode(vec![0u8; 1024 * 1024 + 1]);
    assert_eq!(
        err(json!({"content": [{"type": "image", "content": huge}]})),
        "receipt.content[0]: image is larger than 1048576 bytes"
    );
    let ok = B64.encode(vec![0u8; 700_000]);
    let three = json!({"type": "image", "content": ok});
    assert_eq!(
        err(json!({"content": vec![three; 3]})),
        "receipt has more than 2097152 bytes of images"
    );
    assert_eq!(
        err(json!({"content": [{"type": "image", "content": "not base64!"}]})),
        "receipt.content[0].content is not valid base64"
    );
    assert_eq!(
        err(json!({"content": [{"type": "image"}]})),
        "receipt.content[0].content must be base64 image data"
    );
}

// --- 9 size -------------------------------------------------------------------------------------

#[test]
fn sizes() {
    let s = |v: Value| parse_doc(&json!({"content": [{"type": "text", "text": "x", "size": v}]}));
    for n in 1..=8 {
        assert!(s(json!(n)).is_ok());
    }
    assert!(s(json!([3, 5])).is_ok());
    for bad in [
        json!(0),
        json!(9),
        json!(2.5),
        json!(2.0),
        json!("2"),
        json!([1]),
        json!([1, 2, 3]),
        json!([0, 1]),
        json!([1, 9]),
        json!([1.5, 2]),
        json!(true),
    ] {
        assert_eq!(
            s(bad.clone()).unwrap_err(),
            "receipt.content[0].size must be 1-8 or [w, h]",
            "{bad}"
        );
    }
}

// --- 10-14 layout -------------------------------------------------------------------------------

#[test]
fn alignment_is_padding() {
    for (paper, width, cols) in [(58, 384, 32), (80, 504, 42), (80, 576, 48)] {
        let p = prof(json!({"paper_mm": paper, "width_dots": width}));
        let l = lay(
            json!([
            {"type": "text", "text": "ab"},
            {"type": "text", "text": "ab", "align": "center"},
            {"type": "text", "text": "ab", "align": "right"}]),
            &p,
        );
        let rows = text_rows(&l);
        assert_eq!(rows[0], "ab");
        assert_eq!(rows[1], format!("{}ab", " ".repeat((cols - 2) / 2)));
        assert_eq!(rows[2], format!("{}ab", " ".repeat(cols - 2)));
    }
    let l = lay(
        json!([{"type": "text", "text": "ab", "align": "center", "size": 2}]),
        &Profile::default(),
    );
    // 24 characters a row at size 2
    assert_eq!(text_rows(&l)[0], format!("{}ab", " ".repeat(11)));
    match &l.ops[0] {
        Op::Row { w, h, .. } => assert_eq!((*w, *h), (2, 2)),
        _ => panic!(),
    }
}

#[test]
fn text_wraps_and_keeps_blank_lines() {
    let p = prof(json!({"paper_mm": 58}));
    let l = lay(
        json!([{"type": "text", "text": "aaaa bbbb cccc dddd eeee ffff gggg\n\nz"}]),
        &p,
    );
    assert_eq!(
        text_rows(&l),
        ["aaaa bbbb cccc dddd eeee ffff", "gggg", "", "z"]
    );
    // empty after normalisation: one empty row
    let l = lay(json!([{"type": "text", "text": "\u{200b}"}]), &p);
    assert_eq!(text_rows(&l), [""]);
}

#[test]
fn columns_share_the_rest_and_wrap_per_cell() {
    let p = prof(json!({"paper_mm": 58}));
    let l = lay(
        json!([{"type": "columns", "cells": [
        {"text": "2 x Weißbrot"}, {"text": "5,80 €", "align": "right", "width": 10}]}]),
        &p,
    );
    assert_eq!(
        text_rows(&l),
        [format!("{:<21} {:>10}", "2 x Weißbrot", "5,80 €")]
    );
    // 42 characters: three free cells share 40, the remainder goes to the first
    let p42 = prof(json!({"width_dots": 504}));
    let l = lay(
        json!([{"type": "columns", "cells": [{"text": "a"}, {"text": "b"}, {"text": "c"}]}]),
        &p42,
    );
    assert_eq!(
        text_rows(&l),
        [format!("{:<14} {:<13} {:<13}", "a", "b", "c")]
    );
    // a cell wraps inside its own width and the row is as high as the tallest cell
    let l = lay(
        json!([{"type": "columns", "cells": [
        {"text": "aaa bbb ccc", "width": 7}, {"text": "x", "align": "right"}]}]),
        &p,
    );
    let rows = text_rows(&l);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], format!("{:<7} {:>24}", "aaa bbb", "x"));
    assert_eq!(rows[1], format!("{:<7} {:>24}", "ccc", ""));
    // bold per cell
    let l = lay(
        json!([{"type": "columns", "cells": [{"text": "a", "bold": true}, {"text": "b"}]}]),
        &p,
    );
    match &l.ops[0] {
        Op::Row { runs, .. } => {
            assert_eq!(runs.len(), 2);
            assert!(runs[0].1 && !runs[1].1);
        }
        _ => panic!(),
    }
    // does not fit
    assert_eq!(
        lay_err(
            json!([{"type": "columns", "cells": [{"text": "a", "width": 20}, {"text": "b", "width": 20}]}]),
            &p
        ),
        "receipt.content[0]: columns do not fit 32 characters"
    );
    assert_eq!(
        lay_err(
            json!([{"type": "columns", "size": 2, "cells": [{"text": "a", "width": 15}, {"text": "b", "width": 15}]}]),
            &p
        ),
        "receipt.content[0]: columns do not fit 16 characters"
    );
    // no room left for a free cell
    assert!(
        lay_err(
            json!([{"type": "columns", "cells": [{"text": "a", "width": 31}, {"text": "b"}]}]),
            &p
        )
        .contains("do not fit")
    );
}

#[test]
fn divider_runs_the_whole_width() {
    let l = lay(
        json!([{"type": "divider", "char": "="}]),
        &prof(json!({"paper_mm": 58})),
    );
    assert_eq!(text_rows(&l), ["=".repeat(32)]);
}

#[test]
fn automatic_cut() {
    let t = json!({"type": "text", "text": "x"});
    let k = |content: Value, cut: bool| {
        let d = doc(json!({"content": content, "cut": cut}));
        kinds(&layout(&d, &Profile::default()).unwrap())
    };
    assert_eq!(k(json!([t]), true), ["row", "cut"]);
    assert_eq!(k(json!([t]), false), ["row"]);
    assert_eq!(k(json!([t, {"type": "cut"}]), true), ["row", "cut"]);
    assert_eq!(
        k(json!([t, {"type": "cut"}, t]), true),
        ["row", "cut", "row", "cut"]
    );
    assert_eq!(
        k(json!([t, {"type": "cut"}, {"type": "drawer"}]), true),
        ["row", "cut", "drawer"]
    );
    assert_eq!(k(json!([{"type": "drawer"}]), true), ["drawer"]);
    assert_eq!(k(json!([{"type": "feed"}]), true), ["feed", "cut"]);
    assert_eq!(k(json!([{"type": "cut"}]), true), ["cut"]);
    assert_eq!(
        k(json!([{"type": "drawer"}, t]), true),
        ["drawer", "row", "cut"]
    );
    assert!(!lay(json!([{"type": "drawer"}]), &Profile::default()).has_pages());
    assert!(lay(json!([t]), &Profile::default()).has_pages());
}

#[test]
fn qr_and_barcode_sizes() {
    let p = Profile::default();
    // a short text: 8 dots a module
    let l = lay(json!([{"type": "qr", "data": "hi"}]), &p);
    match &l.ops[0] {
        Op::Image { x, bm } => {
            assert_eq!(bm.w, 29 * 8);
            assert_eq!(*x, (576 - 232) / 2);
        }
        _ => panic!(),
    }
    // a long one shrinks to 2 dots
    let l = lay(
        json!([{"type": "qr", "data": "a".repeat(2000), "ec": "L"}]),
        &p,
    );
    match &l.ops[0] {
        Op::Image { bm, .. } => assert!(bm.w <= 576 && bm.w > 300, "{}", bm.w),
        _ => panic!(),
    }
    let narrow = prof(json!({"paper_mm": 58, "width_dots": 192}));
    assert_eq!(
        lay_err(
            json!([{"type": "qr", "data": "a".repeat(2000), "ec": "L"}]),
            &narrow
        ),
        "receipt.content[0]: qr does not fit 192 dots"
    );
    // an explicit size of 1 is accepted
    assert!(
        layout(
            &doc(
                json!({"content": [{"type": "qr", "data": "a".repeat(2000), "ec": "L", "size": 1}]})
            ),
            &narrow
        )
        .is_ok()
    );
    // 2000 bytes at L and M fit the encoder, at Q and H they do not
    for ec in ["L", "M"] {
        assert!(
            layout(
                &doc(json!({"content": [{"type": "qr", "data": "a".repeat(2000), "ec": ec}]})),
                &p
            )
            .is_ok(),
            "{ec}"
        );
    }
    for ec in ["Q", "H"] {
        assert_eq!(
            lay_err(
                json!([{"type": "qr", "data": "a".repeat(2000), "ec": ec}]),
                &p
            ),
            format!("receipt.content[0]: qr data is too long for ec {ec}")
        );
    }
    // barcodes
    assert_eq!(
        lay_err(
            json!([{"type": "barcode", "format": "code128", "data": "A".repeat(48)}]),
            &p
        ),
        "receipt.content[0]: barcode does not fit 576 dots"
    );
    let l = lay(
        json!([{"type": "barcode", "format": "ean13", "data": "400638133393"}]),
        &p,
    );
    assert_eq!(kinds(&l), ["image", "row", "cut"]);
    // the text row shows the full number with its check digit
    assert_eq!(text_rows(&l)[0].trim(), "4006381333931");
    let l = lay(
        json!([{"type": "barcode", "format": "code128", "data": "123456", "text": false}]),
        &p,
    );
    assert_eq!(kinds(&l), ["image", "cut"]);
}

#[test]
fn images_scale_align_and_stay_inside_their_limits() {
    let p = prof(json!({"paper_mm": 58}));
    let at = |c: Value| match &lay(json!([c]), &p).ops[0] {
        Op::Image { x, bm } => (*x, bm.w, bm.h),
        _ => panic!(),
    };
    // natural size
    assert_eq!(
        at(json!({"type": "image", "content": png_b64(100, 50, 0)})),
        (142, 100, 50)
    );
    assert_eq!(
        at(json!({"type": "image", "content": png_b64(100, 50, 0), "align": "left"})),
        (0, 100, 50)
    );
    assert_eq!(
        at(json!({"type": "image", "content": png_b64(100, 50, 0), "align": "right"})),
        (284, 100, 50)
    );
    // shrunk to the paper, never enlarged
    assert_eq!(
        at(json!({"type": "image", "content": png_b64(768, 100, 0)})),
        (0, 384, 50)
    );
    // width_percent
    assert_eq!(
        at(json!({"type": "image", "content": png_b64(100, 50, 0), "width_percent": 50})),
        (96, 192, 96)
    );
    // transparent PNG: transparent pixels print white
    let mut rgba = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 0]));
    rgba.put_pixel(0, 0, image::Rgba([0, 0, 0, 255]));
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(rgba.as_raw(), 8, 8, image::ExtendedColorType::Rgba8)
        .unwrap();
    let l = lay(
        json!([{"type": "image", "content": B64.encode(out), "align": "left"}]),
        &p,
    );
    match &l.ops[0] {
        Op::Image { bm, .. } => {
            assert_eq!(bm.data[0], 0x80);
            assert!(bm.data[1..].iter().all(|b| *b == 0));
        }
        _ => panic!(),
    }
    // a 1 dot wide image with dither does not panic
    assert!(layout(&doc(json!({"content": [{"type": "image", "content": png_b64(1, 30, 100), "dither": true}]})), &p).is_ok());
    assert!(layout(&doc(json!({"content": [{"type": "image", "content": png_b64(30, 1, 100), "dither": true}]})), &p).is_ok());
    // dither makes mid grey into a pattern
    let l = lay(
        json!([{"type": "image", "content": png_b64(16, 16, 128), "dither": true, "align": "left"}]),
        &p,
    );
    match &l.ops[0] {
        Op::Image { bm, .. } => {
            let black: u32 = bm.data.iter().map(|b| b.count_ones()).sum();
            assert!(black > 60 && black < 200, "{black}");
        }
        _ => panic!(),
    }
    // over 4 megapixels, from the header
    assert_eq!(
        lay_err(
            json!([{"type": "image", "content": png_b64(2001, 2001, 255)}]),
            &p
        ),
        "receipt.content[0]: image is too large (2001x2001, at most 4000000 pixels)"
    );
    assert!(
        lay_err(
            json!([{"type": "image", "content": B64.encode(b"not an image")}]),
            &p
        )
        .starts_with("receipt.content[0]: not a supported image")
    );
}

#[test]
fn a_tall_receipt_stops_before_the_next_element_is_processed() {
    let p = Profile::default();
    // 19000 rows, then the next tall image does not fit what is left
    let tall = json!({"type": "image", "content": png_b64(2, 19000, 0)});
    assert_eq!(
        lay_err(json!([tall.clone(), tall]), &p),
        "receipt is longer than 20000 dots"
    );
    // a tall one that would be scaled up is refused by its target height, before the resize (which
    // would answer "image is too large for this placement" for 200 x 57600 at 16 bytes a pixel)
    assert_eq!(
        lay_err(
            json!([{"type": "image", "content": png_b64(200, 20000, 0), "width_percent": 100}]),
            &p
        ),
        "receipt is longer than 20000 dots"
    );
    // the limit is hit by a feed; the broken image behind it is never decoded
    let broken = json!({"type": "image", "content": B64.encode(b"broken")});
    assert_eq!(
        lay_err(
            json!([{"type": "image", "content": png_b64(2, 19999, 0)}, {"type": "feed", "lines": 20}, broken]),
            &p
        ),
        "receipt is longer than 20000 dots"
    );
    // a broken image on its own does say so
    assert!(lay_err(json!([broken]), &p).contains("not a supported image"));
    // lots of QR codes stop at the limit, not after all of them
    let qr = json!({"type": "qr", "data": "x"});
    assert_eq!(
        lay_err(
            {
                let mut v = vec![json!({"type": "feed", "lines": 20}); 34];
                v.push(qr.clone());
                json!(v)
            },
            &p
        ),
        "receipt is longer than 20000 dots"
    );
    // a tall text is cut off by the same limit
    assert_eq!(
        lay_err(
            rep(
                json!({"type": "text", "text": "a ".repeat(2000), "size": 8}),
                3
            ),
            &prof(json!({"paper_mm": 58}))
        ),
        "receipt is longer than 20000 dots"
    );
}

// --- 3, 15-19 ESC/POS ---------------------------------------------------------------------------

#[test]
fn control_characters_never_reach_the_printer() {
    let evil = "A\u{1b}@B\u{1d}V\u{0}C\u{1b}d\u{9}D";
    let bytes = stream(
        json!([
            {"type": "text", "text": evil},
            {"type": "columns", "cells": [{"text": evil}, {"text": evil, "width": 10}]},
            {"type": "text", "text": "ok"}
        ]),
        &cp1252(),
    );
    // between the header (8 bytes) and the cut and ESC @ (6 bytes) there is text and line feeds only
    let body = &bytes[8..bytes.len() - 6];
    assert!(
        body.iter().all(|b| *b >= 0x20 || *b == 0x0a),
        "{}",
        hex(body)
    );
    assert!(body.starts_with(b"A@BVCd D\n"), "{}", hex(body));
    assert!(body.ends_with(b"ok\n"));
    // barcode text and a divider cannot carry them either
    assert!(
        parse_doc(
            &json!({"content": [{"type": "barcode", "format": "code128", "data": "a\u{1b}"}]})
        )
        .is_err()
    );
}

#[test]
fn the_reference_streams() {
    let s = stream(json!([{"type": "text", "text": "Grüße 5 €"}]), &cp1252());
    assert_eq!(
        hex(&s),
        "1B 40 1B 52 00 1B 74 10 47 72 FC DF 65 20 35 20 80 0A 1D 56 42 00 1B 40"
    );
    let s = stream(json!([{"type": "drawer"}]), &Profile::default());
    assert_eq!(hex(&s), "1B 40 1B 70 00 19 FA 1B 40");
    let s = stream(json!([{"type": "drawer"}]), &cp1252());
    assert_eq!(hex(&s), "1B 40 1B 52 00 1B 74 10 1B 70 00 19 FA 1B 40");
    // every stream ends with ESC @
    for c in [
        json!([{"type": "text", "text": "x"}]),
        json!([{"type": "feed"}]),
        json!([{"type": "qr", "data": "x"}]),
    ] {
        for p in [Profile::default(), cp1252()] {
            let s = stream(c.clone(), &p);
            assert_eq!(&s[..2], &[0x1b, 0x40]);
            assert_eq!(&s[s.len() - 2..], &[0x1b, 0x40]);
        }
    }
}

#[test]
fn size_and_bold_only_on_change() {
    let s = stream(
        json!([
        {"type": "text", "text": "A", "size": 2},
        {"type": "text", "text": "B", "size": 2},
        {"type": "text", "text": "C", "size": 2, "bold": true},
        {"type": "text", "text": "D"},
        {"type": "text", "text": "E", "size": [3, 2]}]),
        &cp1252(),
    );
    let want = "1B 40 1B 52 00 1B 74 10 \
        1D 21 11 41 0A  42 0A  1B 45 01 43 0A  1D 21 00 1B 45 00 44 0A  1D 21 21 45 0A \
        1D 56 42 00 1B 40";
    assert_eq!(unhex(want), s, "{}", hex(&s));
}

#[test]
fn cp1252_draws_the_rows_it_cannot_encode() {
    let s = stream(
        json!([
        {"type": "text", "text": "Hello"},
        {"type": "text", "text": "Привет"},
        {"type": "text", "text": "Bye"}]),
        &cp1252(),
    );
    let (maps, rest) = scan(&s);
    assert_eq!(maps.len(), 1);
    // 6 glyphs, 12 dots each = 72 dots = 9 bytes, one line of 30 rows
    assert_eq!((maps[0].0, maps[0].1), (9, 30));
    let mut want = unhex("1B 40 1B 52 00 1B 74 10");
    want.extend_from_slice(b"Hello\nBye\n");
    want.extend(unhex("1D 56 42 00 1B 40"));
    assert_eq!(rest, want);
    // the order is kept: Hello, the bitmap, Bye
    let at = s
        .windows(4)
        .position(|w| w == [0x1d, 0x76, 0x30, 0x00])
        .unwrap();
    assert!(s[..at].ends_with(b"Hello\n"));
    assert!(s[at..].windows(4).any(|w| w == b"Bye\n"));
    // a mixed row is drawn as a whole row
    let s = stream(
        json!([{"type": "text", "text": "Preis 5 € Привет"}]),
        &cp1252(),
    );
    assert_eq!(scan(&s).0.len(), 1);
}

#[test]
fn cp1252_keeps_a_tall_blank_line_as_tall_as_the_layout_counts() {
    // An empty line feed moves the paper one line at the line spacing (30 dots), not by the
    // character height: a blank line of size 4 is 96 dots in the layout, the row limit, the
    // preview and the PDF, so it goes out as a white bitmap of 96 rows.
    let s = stream(
        json!([{"type": "text", "text": "A\n\nB", "size": 4}]),
        &cp1252(),
    );
    let (maps, rest) = scan(&s);
    assert_eq!(maps.len(), 1, "{rest:?}");
    assert_eq!((maps[0].1, maps[0].2.iter().all(|b| *b == 0)), (96, true));
    assert!(!rest.windows(2).any(|w| w == [0x0a, 0x0a]), "{rest:?}");
    // at size 1 a blank line is one 30 dot feed, as counted: it stays native
    let s = stream(json!([{"type": "text", "text": "A\n\nB"}]), &cp1252());
    let (maps, rest) = scan(&s);
    assert!(maps.is_empty());
    assert!(rest.windows(5).any(|w| {
        w == b"A\n\nB"[..]
            .iter()
            .copied()
            .chain([0x0a])
            .collect::<Vec<_>>()
    }));
}

#[test]
fn raster_mode_sends_no_text_at_all() {
    let s = stream(
        json!([
        {"type": "text", "text": "Grüße 5 €", "bold": true},
        {"type": "text", "text": "x", "size": 2}]),
        &Profile::default(),
    );
    let (maps, rest) = scan(&s);
    // two rows drawn into one bitmap: 30 + 48 rows
    assert_eq!(maps.iter().map(|m| m.1).sum::<usize>(), 78);
    assert_eq!(hex(&rest), "1B 40 1D 56 42 00 1B 40");
    assert!(!s.windows(2).any(|w| w == [0x1b, 0x74] || w == [0x1b, 0x52]));
}

#[test]
fn drawn_rows_merge_and_go_out_in_bands() {
    // 'A' sits in the first 12 dots: two bytes
    let s = stream(json!([{"type": "text", "text": "A"}]), &Profile::default());
    assert_eq!(hex(&s[..10]), "1B 40 1D 76 30 00 02 00 1E 00");
    // five empty rows are 150 white rows: bands of 128 and 22, one byte wide, white kept
    let blank = json!({"type": "text", "text": "\u{200b}"});
    let s = stream(
        json!([blank, blank, blank, blank, blank]),
        &Profile::default(),
    );
    let (maps, _) = scan(&s);
    assert_eq!(
        maps.iter().map(|m| (m.0, m.1)).collect::<Vec<_>>(),
        [(1, 128), (1, 22)]
    );
    assert!(maps.iter().all(|m| m.2.iter().all(|b| *b == 0)));
    assert_eq!(hex(&s[2..10]), "1D 76 30 00 01 00 80 00");
    // trimming is per band: a mark in the second band only widens the second band
    let mut rows = vec![json!({"type": "text", "text": "\u{200b}"}); 5];
    rows.push(json!({"type": "text", "text": format!("{}W", " ".repeat(39))}));
    let (maps, _) = scan(&stream(json!(rows), &Profile::default()));
    assert_eq!((maps[0].0, maps[0].1), (1, 128));
    assert_eq!(maps[0].2.iter().filter(|b| **b != 0).count(), 0);
    assert_eq!(maps[1].0, 60); // 39 blanks in: the glyph W ends in byte 59
    // drawn rows, an image and a QR code are one bitmap; a feed, a cut and a drawer cut it
    let s = stream(
        json!([
        {"type": "text", "text": "a"}, {"type": "qr", "data": "x"}, {"type": "feed", "lines": 2},
        {"type": "text", "text": "b"}, {"type": "drawer"}, {"type": "text", "text": "c"}]),
        &Profile::default(),
    );
    let (maps, rest) = scan(&s);
    // a and the QR code (30 + 232 rows) are one bitmap in three bands, then b and c on their own
    let rows: Vec<usize> = maps.iter().map(|m| m.1).collect();
    assert_eq!(rows, [128, 128, 6, 30, 30]);
    assert_eq!(
        hex(&rest),
        "1B 40 1B 64 02 1B 70 00 19 FA 1D 56 42 00 1B 40"
    );
}

#[test]
fn cut_feed_and_drawer_commands() {
    let t = json!({"type": "text", "text": "\u{200b}"});
    let tail = |p: &Profile, c: Value| {
        let s = stream(c, p);
        let (_, rest) = scan(&s);
        hex(&rest)
    };
    let mut p = Profile::default();
    assert_eq!(tail(&p, json!([t])), "1B 40 1D 56 42 00 1B 40");
    p.cut = CutMode::Full;
    assert_eq!(tail(&p, json!([t])), "1B 40 1D 56 41 00 1B 40");
    p.cut = CutMode::None;
    assert_eq!(tail(&p, json!([t])), "1B 40 1B 64 04 1B 40");
    assert_eq!(
        tail(&p, json!([t, {"type": "cut"}])),
        "1B 40 1B 64 04 1B 40"
    );
    let p = Profile::default();
    assert_eq!(
        tail(&p, json!([{"type": "feed", "lines": 20}])),
        "1B 40 1B 64 14 1D 56 42 00 1B 40"
    );
    assert_eq!(
        tail(&p, json!([{"type": "drawer", "pin": 5}])),
        "1B 40 1B 70 01 19 FA 1B 40"
    );
    assert_eq!(
        tail(
            &p,
            json!([{"type": "drawer"}, {"type": "drawer", "pin": 5}])
        ),
        "1B 40 1B 70 00 19 FA 1B 70 01 19 FA 1B 40"
    );
}

// --- 20-22 goldens ------------------------------------------------------------------------------

#[test]
fn escpos_goldens_of_the_dashboard_sample() {
    for paper in [58, 80] {
        for cp in ["raster", "cp1252"] {
            let p = profile_of(paper, cp);
            let s = stream(sample()["content"].clone(), &p);
            golden_hex(&format!("sample-{paper}-{cp}.hex"), &s);
        }
    }
}

#[test]
fn the_sample_holds_what_a_wrong_setting_would_show() {
    let v = sample();
    let text = v.to_string();
    for c in ["ä", "ö", "ü", "ß", "€"] {
        assert!(text.contains(c), "{c}");
    }
    assert!(
        doc(v.clone())
            .content
            .iter()
            .any(|e| matches!(e, receipt::Element::Columns { .. }))
    );
    let l = lay(v["content"].clone(), &Profile::default());
    assert!(kinds(&l).contains(&"image"));
    // none of the bundle needles of check-bundles
    for needle in ["Nordlicht", "print.example.com", "Datenschutz"] {
        assert!(!text.contains(needle));
    }
}

#[test]
fn the_demo_sample_images_are_what_the_renderer_draws() {
    for paper in [58, 80] {
        let p = prof(json!({"paper_mm": paper}));
        let png = preview_png(&lay(sample()["content"].clone(), &p));
        let path = demo_asset(paper);
        if updating() {
            std::fs::write(&path, &png).unwrap();
        }
        let have = decode_png(&std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path:?}")));
        let want = decode_png(&png);
        assert_eq!(have.dimensions(), want.dimensions(), "{paper} mm");
        assert!(
            have.as_raw() == want.as_raw(),
            "{paper} mm differs (UPDATE_GOLDEN=1 rewrites it)"
        );
    }
}

#[test]
fn print_equals_preview_with_the_raster_code_page() {
    for paper in [58, 80] {
        let p = prof(json!({"paper_mm": paper, "codepage": "raster"}));
        let l = lay(sample()["content"].clone(), &p);
        let (maps, _) = scan(&escpos(&l, &p));
        let preview = decode_png(&preview_png(&l));
        // the bands are one bitmap cut into pieces of the same width: stack them
        let mut y = 0usize;
        for (used, rows, data) in &maps {
            for r in 0..*rows {
                for x in 0..p.width_dots as usize {
                    let byte = x / 8;
                    let black = byte < *used && data[r * used + byte] & (0x80 >> (x % 8)) != 0;
                    let px = preview.get_pixel(x as u32 + 16, (y + r) as u32 + 16).0[0];
                    assert_eq!(px == 0, black, "{paper} mm row {} col {x}", y + r);
                }
            }
            y += rows;
        }
        assert!(y > 300);
    }
}

#[test]
fn the_preview_shows_cuts_and_drawers() {
    let p = Profile::default();
    let l = lay(
        json!([{"type": "text", "text": "x"}, {"type": "cut"}, {"type": "drawer"}]),
        &p,
    );
    let img = decode_png(&preview_png(&l));
    // 16 + 30 + 40 + 30 + 16 rows, 576 + 32 wide, a white border
    assert_eq!(img.dimensions(), (608, 16 + 30 + 40 + 30 + 16));
    assert_eq!(img.get_pixel(0, 0).0[0], 255);
    // the dashed cut line: grey where it is dashed, white in the gaps
    let y = 16 + 30 + 20;
    assert_eq!(img.get_pixel(16, y).0[0], 128);
    assert_eq!(img.get_pixel(16 + 8, y).0[0], 255);
    // the drawer row has grey text
    assert!((16..16 + 576).any(|x| (16 + 70..16 + 94).any(|yy| img.get_pixel(x, yy).0[0] == 128)));
}

// --- 23-24 PDF ----------------------------------------------------------------------------------

fn media_boxes(pdf: &[u8]) -> Vec<(f64, f64)> {
    let t = String::from_utf8_lossy(pdf).into_owned();
    t.match_indices("/MediaBox [0 0 ")
        .map(|(i, m)| {
            let rest = &t[i + m.len()..];
            let end = rest.find(']').unwrap();
            let mut it = rest[..end].split(' ');
            (
                it.next().unwrap().parse().unwrap(),
                it.next().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn pdf_pages_are_always_taller_than_wide() {
    for paper in [58, 80] {
        let p = prof(json!({"paper_mm": paper}));
        let one = pdf(&lay(json!([{"type": "text", "text": "one line"}]), &p), &p).unwrap();
        let boxes = media_boxes(&one);
        assert_eq!(boxes.len(), 1);
        let w = f64::from(paper) * 72.0 / 25.4;
        assert!((boxes[0].0 - w).abs() < 0.01);
        assert!(boxes[0].1 > boxes[0].0, "{paper} mm: {boxes:?}");
        // several cuts: one page each, all taller than wide, a long one grows
        let l = lay(
            json!([
                {"type": "text", "text": "a"}, {"type": "cut"},
                {"type": "text", "text": "b"}, {"type": "cut"},
                {"type": "text", "text": "line\n".repeat(60)}]),
            &p,
        );
        let boxes = media_boxes(&pdf(&l, &p).unwrap());
        assert_eq!(boxes.len(), 3);
        assert!(boxes.iter().all(|b| b.1 > b.0));
        assert!(boxes[2].1 > boxes[0].1 * 2.0, "{boxes:?}");
        assert_eq!(boxes[0], boxes[1]);
    }
    let p = Profile::default();
    let bytes = pdf(&lay(json!([{"type": "text", "text": "x"}]), &p), &p).unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.starts_with("%PDF-1.4"));
    assert!(text.contains("/Count 1"));
    assert!(text.ends_with("%%EOF\n"));
}

#[test]
fn pdf_pages_only_for_sections_with_something_to_print() {
    let p = Profile::default();
    let t = json!({"type": "text", "text": "x"});
    let n = |c: Value| media_boxes(&pdf(&lay(c, &p), &p).unwrap()).len();
    assert_eq!(n(json!([{"type": "cut"}, t])), 1);
    assert_eq!(n(json!([t, {"type": "cut"}, {"type": "cut"}, t])), 2);
    assert_eq!(
        n(json!([t, {"type": "cut"}, {"type": "feed", "lines": 3}])),
        1
    );
    assert_eq!(n(json!([t, {"type": "drawer"}])), 1);
    assert_eq!(n(json!([{"type": "qr", "data": "x"}])), 1);
    for c in [
        json!([{"type": "drawer"}]),
        json!([{"type": "feed"}]),
        json!([{"type": "cut"}, {"type": "cut"}]),
    ] {
        assert_eq!(
            pdf(&lay(c, &p), &p).unwrap_err(),
            "receipt prints nothing on a PDF printer"
        );
    }
}

#[test]
fn a_head_wider_than_the_usual_print_width_renders_everywhere() {
    // 80 mm paper with a 640 dot head: 53 characters, 80 bytes a raster row
    let p = prof(json!({"paper_mm": 80, "width_dots": 640}));
    let content = json!([
        {"type": "text", "text": "x".repeat(60)},
        {"type": "qr", "data": "https://example.com/r/4711"}]);
    let l = lay(content, &p);
    assert_eq!(l.width_dots, 640);
    assert_eq!(text_rows(&l)[0].chars().count(), 53);
    let s = stream(json!([{"type": "divider", "char": "#"}]), &p);
    assert_eq!(&s[2..8], &[0x1d, 0x76, 0x30, 0x00, 80, 0], "{s:?}");
    let img = decode_png(&preview_png(&l));
    assert_eq!(img.width(), 640 + 32);
    assert!(!media_boxes(&pdf(&l, &p).unwrap()).is_empty());
}

/// (first dot, last dot + 1) of the text of the row after a bitmap, for the rows of a layout that
/// is one bitmap followed by label rows.
fn label_span(l: &Layout) -> ((u32, u32), Vec<(u32, u32)>) {
    let Op::Image { x, bm } = &l.ops[0] else {
        panic!("{:?}", l.ops[0])
    };
    let rows = text_rows(l)
        .iter()
        .map(|r| {
            let start = r.chars().take_while(|c| *c == ' ').count() as u32 * 12;
            (start, r.chars().count() as u32 * 12)
        })
        .collect();
    ((*x, x + bm.w), rows)
}

#[test]
fn the_barcode_text_sits_under_the_bars_whatever_the_alignment() {
    let p = Profile::default();
    for (align, centred) in [("left", false), ("center", true), ("right", false)] {
        for (format, data) in [("ean13", "4006381333931"), ("code128", "ABC-12")] {
            let l = lay(
                json!([{"type": "barcode", "format": format, "data": data, "align": align}]),
                &p,
            );
            let ((from, to), rows) = label_span(&l);
            assert_eq!(rows.len(), 1);
            let (start, end) = rows[0];
            assert!(
                start >= from.saturating_sub(11) && end <= to,
                "{align} {data}: bars {from}-{to}, text {start}-{end}"
            );
            if centred {
                // centre of the text within one cell of the centre of the bars
                let (mid_bars, mid_text) = ((from + to) / 2, (start + end) / 2);
                assert!(mid_bars.abs_diff(mid_text) <= 12, "{mid_bars} {mid_text}");
            }
        }
    }
    // right: the text is not on the left half of the paper
    let l = lay(
        json!([{"type": "barcode", "format": "ean13", "data": "4006381333931", "align": "right"}]),
        &p,
    );
    assert!(label_span(&l).1[0].0 > 288);
}

#[test]
fn a_barcode_text_longer_than_the_line_wraps_in_both_code_pages() {
    let digits: String = "1234567890".repeat(5)[..48].to_string();
    for cp in ["raster", "cp1252"] {
        let p = prof(json!({"paper_mm": 58, "codepage": cp}));
        let el = json!([{"type": "barcode", "format": "code128", "data": digits, "size": 1}]);
        let l = lay(el.clone(), &p);
        let rows = text_rows(&l);
        assert_eq!(rows.len(), 2, "{cp}: {rows:?}");
        assert!(
            rows.iter().all(|r| r.chars().count() <= 32),
            "{cp}: {rows:?}"
        );
        // nothing is lost: the whole number is there, in order
        let all: String = rows.iter().map(|r| r.trim()).collect();
        assert_eq!(all, digits, "{cp}");
        // the layout counted both rows: the bars, then two lines
        let s = escpos(&l, &p);
        if cp == "cp1252" {
            // two native lines, the first a whole line of the printer, each ended by its own LF
            let has = |t: &[u8]| s.windows(t.len()).any(|w| w == t);
            assert!(has(&[&digits.as_bytes()[..32], &[0x0a]].concat()));
            assert!(has(&[&digits.as_bytes()[32..], &[0x0a]].concat()));
        }
    }
}

#[test]
fn a_cut_without_a_cutter_counts_its_feed_against_the_length_limit() {
    // cut: none writes ESC d 4 for every cut, four lines of 30 dots, so 500 cuts are 7.5 m of
    // paper, not 0. With a cutter a cut feeds nothing that is counted.
    let none = prof(json!({"cut": "none"}));
    let cut = json!({"type": "cut"});
    assert!(
        layout(
            &doc(json!({"cut": false, "content": rep(cut.clone(), 166)})),
            &none
        )
        .is_ok()
    );
    assert_eq!(
        lay_err(rep(cut.clone(), 167), &none),
        "receipt is longer than 20000 dots"
    );
    assert!(
        layout(
            &doc(json!({"content": rep(cut.clone(), 500)})),
            &Profile::default()
        )
        .is_ok()
    );
    // the cut added at the end feeds as well: 166 cuts fit (19920), a line of text and the
    // automatic cut do not
    let mut c = vec![cut.clone(); 166];
    c.push(json!({"type": "text", "text": "x"}));
    assert_eq!(
        lay_err(Value::Array(c), &none),
        "receipt is longer than 20000 dots"
    );
}

// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Normalized order -> packing-slip PDF — the port of `app/packing_slip.py`.
//!
//! ponytail: a deliberately small PDF writer — Helvetica/WinAnsi text and hairlines, no images,
//! no embedded fonts, no compression, and column widths estimated at 0.58 em per character
//! instead of real font metrics. Enough for a packing slip; for a logo, a non-Latin script or
//! real typography, render elsewhere and submit as pdf_uri.
//!
//! The output is byte-identical to the Python renderer for the same order (golden files in the
//! tests).
use crate::orders::{Order, py_strip};

pub const A4: (u32, u32) = (595, 842);

const M: i64 = 40; // page margin
const LEAD: i64 = 14; // line height
/// Rough Helvetica advance per character. Deliberately above the ~0.5 em average (digits and
/// capitals are 0.556–0.72): over-estimating truncates a little early and right-aligns a little
/// left, which keeps columns from colliding. Under-estimating would overlap them.
const EM: f64 = 0.58;

/// cp1252 bytes 0x80..=0x9F as code points; 0 = undefined in Python's codec.
const CP1252_HIGH: [u16; 32] = [
    0x20ac, 0, 0x201a, 0x192, 0x201e, 0x2026, 0x2020, 0x2021, 0x2c6, 0x2030, 0x160, 0x2039, 0x152,
    0, 0x17d, 0, 0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x2dc, 0x2122, 0x161,
    0x203a, 0x153, 0, 0x17e, 0x178,
];

/// One character as a WinAnsi byte, `?` when cp1252 has no such character
/// (`str.encode("cp1252", "replace")`).
fn cp1252(c: char) -> u8 {
    match c as u32 {
        u @ (0..=0x7f | 0xa0..=0xff) => u as u8,
        u => CP1252_HIGH
            .iter()
            .position(|&h| h != 0 && u32::from(h) == u)
            .map_or(b'?', |i| 0x80 + i as u8),
    }
}

/// Text -> PDF string body: WinAnsi-encodable, with (, ) and \ escaped.
fn esc(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        let b = cp1252(c);
        if matches!(b, b'\\' | b'(' | b')') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out
}

fn width(s: &str, size: i64) -> f64 {
    s.chars().count() as f64 * EM * size as f64
}

/// Truncate to what fits in `avail` points (ellipsis if cut).
fn fit(s: &str, avail: i64, size: i64) -> String {
    let limit = ((avail as f64 / (EM * size as f64)) as usize).max(1);
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(limit - 1).collect();
    cut.push('…');
    cut
}

/// `{x:.0f}`; `{:.0}` rounds the exact binary value half-to-even, as Python does.
fn n0(x: f64) -> String {
    format!("{x:.0}")
}

fn text(x: f64, y: i64, s: &str, size: i64, bold: bool) -> Vec<u8> {
    let mut out = format!(
        "BT /{} {size} Tf {} {y} Td (",
        if bold { "F2" } else { "F1" },
        n0(x)
    )
    .into_bytes();
    out.extend(esc(s));
    out.extend(b") Tj ET\n");
    out
}

fn right(x: i64, y: i64, s: &str, size: i64, bold: bool) -> Vec<u8> {
    text(x as f64 - width(s, size), y, s, size, bold)
}

fn rule(x0: i64, y: i64, x1: i64) -> Vec<u8> {
    format!("0.5 w {x0} {y} m {x1} {y} l S\n").into_bytes()
}

/// Assemble content streams into a PDF: catalog, page tree, two fonts, then page+stream pairs.
fn pdf(streams: &[Vec<u8>], (w, h): (u32, u32)) -> Vec<u8> {
    let kids: Vec<String> = (0..streams.len())
        .map(|i| format!("{} 0 R", 5 + 2 * i))
        .collect();
    let mut objs: Vec<Result<String, &[u8]>> = vec![
        Ok("<< /Type /Catalog /Pages 2 0 R >>".into()),
        Ok(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), streams.len())),
        Ok("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .into()),
        Ok("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .into()),
    ];
    for (i, stream) in streams.iter().enumerate() {
        objs.push(Ok(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources \
             << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i + 1
        )));
        objs.push(Err(stream));
    }

    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (n, obj) in objs.iter().enumerate() {
        offsets.push(out.len());
        match obj {
            Err(data) => {
                out.extend(
                    format!("{} 0 obj\n<< /Length {} >>\nstream\n", n + 1, data.len()).bytes(),
                );
                out.extend(*data);
                out.extend(b"\nendstream\nendobj\n");
            }
            Ok(body) => out.extend(format!("{} 0 obj\n{body}\nendobj\n", n + 1).bytes()),
        }
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

/// Render a normalized order as a one- or many-page A4 PDF titled "Packing slip".
pub fn render_packing_slip(order: &Order) -> Vec<u8> {
    render(order, A4, "Packing slip")
}

/// `render_packing_slip` with an explicit page size and title.
pub fn render(order: &Order, page: (u32, u32), title: &str) -> Vec<u8> {
    let (w, h) = (i64::from(page.0), i64::from(page.1));
    let num = py_strip(&order.number);
    let col_sku = M + 34;
    let col_name = M + 110;
    let name_width = w - M - col_name - 82; // last 70pt are the amount, plus a 12pt gutter
    let floor = M + 40; // below this a page is full
    let some = |o: &Option<String>| o.clone().filter(|v| !v.is_empty());
    let note = some(&order.note);

    let mut pages: Vec<Vec<u8>> = Vec::new();
    let mut body: Vec<u8> = Vec::new();
    let mut y: i64 = 0;

    let open_page = |first: bool, body: &mut Vec<u8>, y: &mut i64| {
        body.clear();
        if first {
            body.extend(text(M as f64, h - M - 14, title, 18, true));
            if let Some(shop) = some(&order.shop) {
                body.extend(right(w - M, h - M - 14, &shop, 10, false));
            }
            let heading = if num.is_empty() {
                "Order".to_string()
            } else {
                format!("Order {num}")
            };
            body.extend(text(M as f64, h - M - 40, &heading, 12, true));
            if let Some(date) = some(&order.date) {
                body.extend(right(w - M, h - M - 40, &date, 10, false));
            }
            *y = h - M - 66;
            let who = [order.customer.clone()]
                .into_iter()
                .chain(order.address.iter().cloned().map(Some))
                .chain([order.email.clone(), order.phone.clone()]);
            for line in who.flatten().filter(|l| !l.is_empty()) {
                body.extend(text(M as f64, *y, &fit(&line, w - 2 * M, 10), 10, false));
                *y -= LEAD;
            }
            *y -= 12;
        } else {
            body.extend(text(
                M as f64,
                h - M - 14,
                &format!("Order {num} (continued)"),
                12,
                true,
            ));
            *y = h - M - 44;
        }
        body.extend(text(M as f64, *y, "Qty", 10, true));
        body.extend(text(col_sku as f64, *y, "SKU", 10, true));
        body.extend(text(col_name as f64, *y, "Item", 10, true));
        body.extend(right(w - M, *y, "Amount", 10, true));
        *y -= 5;
        body.extend(rule(M, *y, w - M));
        *y -= LEAD;
    };

    open_page(true, &mut body, &mut y);
    for item in &order.lines {
        if y < floor {
            pages.push(std::mem::take(&mut body));
            open_page(false, &mut body, &mut y);
        }
        let get = |o: &Option<String>| o.clone().unwrap_or_default();
        body.extend(text(M as f64, y, &get(&item.qty), 10, false));
        body.extend(text(
            col_sku as f64,
            y,
            &fit(&get(&item.sku), col_name - col_sku - 6, 10),
            10,
            false,
        ));
        body.extend(text(
            col_name as f64,
            y,
            &fit(&get(&item.name), name_width, 10),
            10,
            false,
        ));
        if let Some(total) = &item.total {
            body.extend(right(w - M, y, total, 10, false));
        }
        y -= LEAD;
    }

    let totals = &order.totals;
    let needed = totals.len() as i64 * LEAD + if note.is_some() { 2 * LEAD } else { 0 } + 20;
    if !totals.is_empty() || note.is_some() {
        if y - needed < M {
            // totals must not be orphaned mid-page
            pages.push(std::mem::take(&mut body));
            open_page(false, &mut body, &mut y);
        }
        y -= 5;
        body.extend(rule(w - M - 200, y, w - M));
        y -= LEAD + 2;
    }
    for (i, (label, amount)) in totals.iter().enumerate() {
        let last = i == totals.len() - 1;
        body.extend(text((w - M - 200) as f64, y, label, 10, last));
        body.extend(right(w - M, y, amount, 10, last));
        y -= LEAD;
    }
    if let Some(note) = &note {
        y -= LEAD;
        // ponytail: notes are a single truncated line — wrap them if shops start writing essays.
        body.extend(text(
            M as f64,
            y,
            &fit(&format!("Note: {note}"), w - 2 * M, 10),
            10,
            false,
        ));
    }
    pages.push(body);

    let count = pages.len();
    let stamped: Vec<Vec<u8>> = pages
        .into_iter()
        .enumerate()
        .map(|(i, mut p)| {
            p.extend(text(
                M as f64,
                M - 16,
                &format!("Page {}/{count}", i + 1),
                8,
                false,
            ));
            p
        })
        .collect();
    pdf(&stamped, page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::OrderLine;

    fn order() -> Order {
        let line = |q: &str, sku: &str, name: &str, total: &str| OrderLine {
            qty: Some(q.into()),
            sku: Some(sku.into()),
            name: Some(name.into()),
            total: Some(total.into()),
        };
        Order {
            number: "1001".into(),
            date: Some("2026-07-25".into()),
            shop: Some("Acme Store".into()),
            customer: Some("Jane Doe".into()),
            address: vec!["Musterweg 1".into(), "12345 Berlin".into(), "DE".into()],
            lines: vec![
                line("2", "A-1", "Widget", "19.80"),
                line("1", "B-2", "Gadget (large)", "9.90"),
            ],
            totals: vec![
                ("Subtotal".into(), "29.70".into()),
                ("Total".into(), "34.60".into()),
            ],
            note: Some("leave at the door".into()),
            ..Default::default()
        }
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    /// The content streams joined, as Latin-1 (like the Python tests).
    fn streams(pdf: &[u8]) -> String {
        let mut out = String::new();
        let mut rest = pdf;
        while let Some(i) = find(rest, b"\nstream\n") {
            rest = &rest[i + 8..];
            let end = find(rest, b"endstream").unwrap();
            out.extend(rest[..end].iter().map(|&b| b as char));
            rest = &rest[end..];
        }
        out
    }

    fn number_after(pdf: &[u8], key: &[u8]) -> usize {
        let at = find(pdf, key).unwrap() + key.len();
        let digits: Vec<u8> = pdf[at..]
            .iter()
            .copied()
            .skip_while(u8::is_ascii_whitespace)
            .take_while(u8::is_ascii_digit)
            .collect();
        String::from_utf8(digits).unwrap().parse().unwrap()
    }

    #[test]
    fn renders_a_structurally_valid_pdf() {
        let pdf = render_packing_slip(&order());
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.trim_ascii_end().ends_with(b"%%EOF"));
        // startxref must point at the xref table, or a reader rejects the file
        let offset = number_after(&pdf, b"startxref");
        assert_eq!(&pdf[offset..offset + 4], b"xref");
        // every object offset in the xref table must land on that object's header
        let table = &pdf[offset..];
        let objects = number_after(table, b"xref\n0 ");
        let rows: Vec<&[u8]> = table.split(|&b| b == b'\n').skip(2).take(objects).collect();
        assert!(rows[0].starts_with(b"0000000000 65535 f"));
        for (i, row) in rows.iter().enumerate().skip(1) {
            let off: usize = std::str::from_utf8(&row[..10]).unwrap().parse().unwrap();
            assert!(pdf[off..].starts_with(format!("{i} 0 obj").as_bytes()));
        }
        assert_eq!(count(&pdf, b" 0 obj\n"), objects - 1);
    }

    #[test]
    fn order_content_is_on_the_page() {
        let text = streams(&render_packing_slip(&order()));
        for expected in [
            "1001",
            "Jane Doe",
            "Musterweg 1",
            "Widget",
            "A-1",
            "34.60",
            "leave at the door",
            "Acme Store",
        ] {
            assert!(text.contains(expected), "{expected}");
        }
    }

    #[test]
    fn escapes_pdf_syntax_and_non_latin_characters() {
        let mut o = order();
        o.customer = Some(r"Foo (Bar) \ Baz".into());
        o.note = Some("Grüße 日本".into());
        let text = streams(&render_packing_slip(&o));
        assert!(text.contains(r"Foo \(Bar\) \\ Baz"));
        assert!(text.contains("Gr\u{fc}\u{df}e")); // cp1252 round-trip
        assert!(text.contains("??")); // unmappable glyphs degrade, never crash
    }

    #[test]
    fn long_orders_paginate_and_number_the_pages() {
        let mut o = order();
        o.lines = (0..120)
            .map(|i| OrderLine {
                qty: Some("1".into()),
                sku: Some(format!("S-{i}")),
                name: Some(format!("Item {i}")),
                total: Some("1.00".into()),
            })
            .collect();
        let pdf = render_packing_slip(&o);
        let text = streams(&pdf);
        let pages = number_after(&pdf, b"/Count ");
        assert_ne!(pages, 1);
        assert!(text.contains("Page 1/") && text.contains("S-119")); // last row survived
        assert!((0..120).all(|i| text.contains(&format!("(S-{i})"))));
        assert!(text.contains(&format!("Page {pages}/{pages}")));
    }

    #[test]
    fn missing_fields_do_not_crash() {
        let pdf = render_packing_slip(&Order {
            number: "7".into(),
            ..Default::default()
        });
        assert!(pdf.starts_with(b"%PDF"));
        assert!(find(&pdf, b"/Count 1").is_some());
    }

    #[test]
    fn cp1252_matches_pythons_codec() {
        assert_eq!(cp1252('A'), b'A');
        assert_eq!(cp1252('€'), 0x80);
        assert_eq!(cp1252('…'), 0x85);
        assert_eq!(cp1252('Ÿ'), 0x9f);
        assert_eq!(cp1252('ß'), 0xdf);
        assert_eq!(cp1252('\u{a0}'), 0xa0);
        // undefined bytes and the C1 controls are not encodable
        assert_eq!(cp1252('\u{81}'), b'?');
        assert_eq!(cp1252('\u{9f}'), b'?');
        assert_eq!(cp1252('日'), b'?');
        assert_eq!(esc("a(b)\\😀"), b"a\\(b\\)\\\\?"); // one '?' per code point
    }

    #[test]
    fn fit_truncates_on_characters_with_an_ellipsis() {
        assert_eq!(fit("abc", 100, 10), "abc");
        assert_eq!(fit(&"é".repeat(40), 58, 10), format!("{}…", "é".repeat(9))); // limit 10
        assert_eq!(fit("abcdef", 0, 10), "…"); // limit floors at 1
        assert_eq!(fit("abcdef", -5, 10), "…");
    }

    /// `{:.0}` must round like Python's `{x:.0f}` on thousands of coordinates, ties included.
    #[test]
    fn n0_rounds_like_python() {
        let cases: Vec<(String, String)> =
            serde_json::from_str(include_str!("../tests/fixtures/packing_slip/n0_cases.json"))
                .unwrap();
        assert!(cases.len() > 4000);
        for (x, expected) in cases {
            assert_eq!(n0(x.parse().unwrap()), expected, "x = {x}");
        }
    }
}

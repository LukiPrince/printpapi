// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! QR codes and barcodes as bitmaps. They are always raster (`GS v 0`): the preview needs the
//! encoders anyway, and print and preview are then the same dots.
//!
//! - `qrcodegen` 1.8.0: <https://docs.rs/qrcodegen/1.8.0/qrcodegen/struct.QrCode.html>,
//!   <https://docs.rs/qrcodegen/1.8.0/qrcodegen/enum.DataTooLong.html>
//! - `barcoders` 2.0.0 (Code 128 tables and EAN-13): <https://docs.rs/crate/barcoders/2.0.0/source/>

use super::raster::{blank, fill};
use super::{BarFormat, Ec};
use crate::label::Bitmap;
use barcoders::sym::code128::Code128;
use barcoders::sym::ean13::EAN13;
use qrcodegen::{QrCode, QrCodeEcc, QrSegment, Version};

/// Quiet zone of a QR code, in modules.
const QR_QUIET: u32 = 4;
/// Quiet zone of a barcode on each side, in modules.
const BAR_QUIET: u32 = 10;
pub const CODE128_MAX: usize = 48;

/// A QR code with its quiet zone. `size` is dots per module; absent, the largest of 8 down to 2
/// that fits `max_dots`. As with `QrCode::encode_text` (qrcodegen 1.8.0, `encode_segments`), the level
/// is raised when that costs no version: `ec` is the least that is used, and it alone decides when
/// the data is too long.
pub fn qr(data: &str, ec: Ec, size: Option<u32>, max_dots: u32) -> Result<Bitmap, String> {
    let level = match ec {
        Ec::L => QrCodeEcc::Low,
        Ec::M => QrCodeEcc::Medium,
        Ec::Q => QrCodeEcc::Quartile,
        Ec::H => QrCodeEcc::High,
    };
    let segs = QrSegment::make_segments(data);
    let code =
        QrCode::encode_segments_advanced(&segs, level, Version::MIN, Version::MAX, None, true)
            .map_err(|_| format!("qr data is too long for ec {}", ec.letter()))?;
    let modules = code.size() as u32;
    let total = modules + 2 * QR_QUIET;
    let scale = match size {
        Some(s) => (total * s <= max_dots).then_some(s),
        None => (2..=8).rev().find(|s| total * s <= max_dots),
    }
    .ok_or_else(|| format!("qr does not fit {max_dots} dots"))?;
    let mut bm = blank(total * scale, total * scale);
    for y in 0..modules {
        for x in 0..modules {
            if code.get_module(x as i32, y as i32) {
                fill(
                    &mut bm,
                    (x + QR_QUIET) * scale,
                    (y + QR_QUIET) * scale,
                    scale,
                    scale,
                );
            }
        }
    }
    Ok(bm)
}

/// The data of a barcode element is valid for its format. The error is the tail of a path
/// message: `.data must ...` or `: ean13 check digit is wrong`.
pub fn check_barcode(format: BarFormat, data: &str) -> Result<(), String> {
    match format {
        BarFormat::Code128 => {
            let n = data.chars().count();
            if n == 0 || n > CODE128_MAX || !data.chars().all(|c| (' '..='~').contains(&c)) {
                return Err(format!(
                    ".data must be 1 to {CODE128_MAX} characters of printable ASCII"
                ));
            }
            Ok(())
        }
        BarFormat::Ean13 => ean13_digits(data).map(|_| ()),
    }
}

/// The 13 digits of an EAN-13 (the check digit added when only 12 were given).
pub fn ean13_digits(data: &str) -> Result<String, String> {
    let bytes = data.as_bytes();
    if !(bytes.len() == 12 || bytes.len() == 13) || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(".data must be 12 or 13 digits".into());
    }
    let d: Vec<u32> = bytes.iter().map(|b| u32::from(b - b'0')).collect();
    let sum: u32 = d[..12]
        .iter()
        .enumerate()
        .map(|(i, v)| if i % 2 == 0 { *v } else { v * 3 })
        .sum();
    let check = (10 - sum % 10) % 10;
    if d.len() == 13 && d[12] != check {
        return Err(": ean13 check digit is wrong".into());
    }
    Ok(format!("{}{}", &data[..12], check))
}

/// The bars of a barcode as modules (1 = bar), without quiet zone.
pub fn modules(format: BarFormat, data: &str) -> Result<Vec<u8>, String> {
    check_barcode(format, data).map_err(|e| format!("barcode{e}"))?;
    match format {
        BarFormat::Code128 => {
            // All digits and an even count: set C (two digits a symbol), otherwise set B.
            // ponytail: no mixed-set optimisation.
            let set_c = data.len().is_multiple_of(2) && data.bytes().all(|b| b.is_ascii_digit());
            let prefixed = format!("{}{data}", if set_c { 'Ć' } else { 'Ɓ' });
            Ok(Code128::new(prefixed)
                .map_err(|e| format!("barcode: {e}"))?
                .encode())
        }
        BarFormat::Ean13 => {
            let digits = ean13_digits(data)?;
            Ok(EAN13::new(&digits[..12])
                .map_err(|e| format!("barcode: {e}"))?
                .encode())
        }
    }
}

/// A barcode with its quiet zones. `size` is dots per module; absent, the largest of 3 down to
/// 2 that fits `max_dots`.
pub fn barcode(
    format: BarFormat,
    data: &str,
    height: u32,
    size: Option<u32>,
    max_dots: u32,
) -> Result<Bitmap, String> {
    let mods = modules(format, data)?;
    let total = mods.len() as u32 + 2 * BAR_QUIET;
    let scale = match size {
        Some(s) => (total * s <= max_dots).then_some(s),
        None => (2..=3).rev().find(|s| total * s <= max_dots),
    }
    .ok_or_else(|| format!("barcode does not fit {max_dots} dots"))?;
    let mut bm = blank(total * scale, height);
    for (i, m) in mods.iter().enumerate() {
        if *m == 1 {
            fill(&mut bm, (BAR_QUIET + i as u32) * scale, 0, scale, height);
        }
    }
    Ok(bm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modules_of(bm: &Bitmap, scale: u32) -> Vec<Vec<bool>> {
        (0..bm.h / scale)
            .map(|my| {
                (0..bm.w / scale)
                    .map(|mx| {
                        let (x, y) = (mx * scale, my * scale);
                        bm.data[y as usize * bm.bytes_per_row + x as usize / 8] & (0x80 >> (x % 8))
                            != 0
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn qr_matches_the_encoder_with_a_quiet_zone() {
        let bm = qr("https://example.com/r/4711", Ec::M, Some(2), 384).unwrap();
        let code = QrCode::encode_text("https://example.com/r/4711", QrCodeEcc::Medium).unwrap();
        let n = code.size() as u32;
        assert_eq!(bm.w, (n + 8) * 2);
        let m = modules_of(&bm, 2);
        for y in 0..n + 8 {
            for x in 0..n + 8 {
                let inside = (4..n + 4).contains(&x) && (4..n + 4).contains(&y);
                let want = inside && code.get_module((x - 4) as i32, (y - 4) as i32);
                assert_eq!(m[y as usize][x as usize], want, "{x},{y}");
            }
        }
    }

    #[test]
    fn qr_takes_the_free_error_correction_boost_of_encode_text() {
        // "hi" at M fits version 1 at H as well: encode_text (qrcodegen 1.8.0, lib.rs
        // `encode_segments` with boostecl = true) raises the level when the version stays
        let code = QrCode::encode_text("hi", QrCodeEcc::Medium).unwrap();
        assert_eq!(code.error_correction_level(), QrCodeEcc::High);
        let bm = qr("hi", Ec::M, Some(2), 384).unwrap();
        let n = code.size() as u32;
        let m = modules_of(&bm, 2);
        for y in 0..n {
            for x in 0..n {
                assert_eq!(
                    m[(y + 4) as usize][(x + 4) as usize],
                    code.get_module(x as i32, y as i32),
                    "{x},{y}"
                );
            }
        }
    }

    #[test]
    fn qr_auto_size_takes_the_largest_that_fits_and_shrinks() {
        let small = qr("hi", Ec::M, None, 384).unwrap();
        assert_eq!(small.w, small.h);
        // version 1 is 21 modules: (21 + 8) * 8 dots = 232
        assert_eq!(small.w, 29 * 8);
        let big = "a".repeat(2000);
        let n = QrCode::encode_text(&big, QrCodeEcc::Low).unwrap().size() as u32 + 8;
        assert!(n * 3 > 384 && n * 2 <= 384, "{n} modules");
        assert_eq!(qr(&big, Ec::L, None, 384).unwrap().w, n * 2);
        // too wide even at 2 dots
        assert_eq!(
            qr(&big, Ec::L, None, n * 2 - 1).unwrap_err(),
            format!("qr does not fit {} dots", n * 2 - 1)
        );
        // an explicit size of 1 is accepted where 2 is not
        assert!(qr(&big, Ec::L, Some(1), n).is_ok());
        assert!(qr(&big, Ec::L, Some(2), n).is_err());
    }

    #[test]
    fn qr_levels_and_the_data_limit() {
        // Capacity by level at version 40, bytes: L 2953, M 2331, Q 1663, H 1273
        let data = "a".repeat(2000);
        assert!(qr(&data, Ec::L, Some(1), 600).is_ok());
        assert!(qr(&data, Ec::M, Some(1), 600).is_ok());
        assert_eq!(
            qr(&data, Ec::Q, Some(1), 600).unwrap_err(),
            "qr data is too long for ec Q"
        );
        assert_eq!(
            qr(&data, Ec::H, Some(1), 600).unwrap_err(),
            "qr data is too long for ec H"
        );
    }

    #[test]
    fn digits_use_the_denser_numeric_mode() {
        // the encoder chooses the segments: 2000 digits fit even at H (numeric mode, 1.7 bits a digit)
        let digits = "7".repeat(2000);
        assert!(qr(&digits, Ec::H, Some(1), 600).is_ok());
    }

    #[test]
    fn ean13_check_digit() {
        assert_eq!(ean13_digits("400638133393").unwrap(), "4006381333931");
        assert_eq!(ean13_digits("4006381333931").unwrap(), "4006381333931");
        assert_eq!(
            ean13_digits("4006381333930").unwrap_err(),
            ": ean13 check digit is wrong"
        );
        for bad in [
            "",
            "12345678901",
            "40063813339312",
            "40063813339a",
            "４００６３８１３３３９３１",
        ] {
            assert_eq!(
                ean13_digits(bad).unwrap_err(),
                ".data must be 12 or 13 digits",
                "{bad}"
            );
        }
    }

    #[test]
    fn ean13_has_95_modules() {
        let m = modules(BarFormat::Ean13, "4006381333931").unwrap();
        assert_eq!(m.len(), 95);
        assert_eq!(&m[..3], &[1, 0, 1]);
        assert_eq!(&m[92..], &[1, 0, 1]);
    }

    #[test]
    fn code128_sets_and_limits() {
        // set C: 2 digits a symbol: start + 3 pairs + check + stop (13 modules) = 11 * 5 + 13
        assert_eq!(
            modules(BarFormat::Code128, "123456").unwrap().len(),
            11 * 5 + 13
        );
        // odd digit count: set B, a symbol a digit
        assert_eq!(
            modules(BarFormat::Code128, "12345").unwrap().len(),
            11 * 7 + 13
        );
        // a single digit is still fine
        assert!(modules(BarFormat::Code128, "7").is_ok());
        // every printable ASCII character encodes in set B
        let all: String = (' '..='~').collect();
        for c in all.chars() {
            assert!(modules(BarFormat::Code128, &c.to_string()).is_ok(), "{c:?}");
        }
        for s in [all.as_str(), "\u{1b}", "ü", "", &"a".repeat(49)] {
            assert!(check_barcode(BarFormat::Code128, s).is_err(), "{s:?}");
        }
        // the barcoders switch characters never come from the data
        assert!(modules(BarFormat::Code128, "Ć1234").is_err());
    }

    #[test]
    fn barcode_sizes() {
        // 48 set B characters are 563 modules: never on 576 dots
        let long = "A".repeat(48);
        assert_eq!(
            barcode(BarFormat::Code128, &long, 80, None, 576).unwrap_err(),
            "barcode does not fit 576 dots"
        );
        let b = barcode(BarFormat::Ean13, "4006381333931", 80, None, 576).unwrap();
        assert_eq!(b.w, (95 + 20) * 3);
        assert_eq!(b.h, 80);
        // 384 dots: size 3 is 345, still fits; the quiet zone is white
        let narrow = barcode(BarFormat::Ean13, "4006381333931", 80, None, 300).unwrap();
        assert_eq!(narrow.w, (95 + 20) * 2);
        assert!(
            barcode(BarFormat::Ean13, "4006381333931", 80, Some(4), 400).is_err(),
            "4 dots a module is 460"
        );
        assert_eq!(b.data[0], 0);
    }
}

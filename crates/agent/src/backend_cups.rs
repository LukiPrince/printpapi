// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Linux and macOS (CUPS underneath): `lp` for printing, `lpoptions` for capabilities.

use crate::config::Capabilities;
use crate::print::{Options, SendResult, lp_options};
use crate::proc::{Cmd, Runner};

fn lp(printer: &str, extra: &[&str], more: Vec<String>, data: &[u8], run: &Runner) -> SendResult {
    let mut argv: Vec<String> = vec!["lp".into(), "-d".into(), printer.into()];
    argv.extend(extra.iter().map(|s| s.to_string()));
    argv.extend(more);
    let mut cmd = Cmd::new(argv, 60);
    cmd.input = Some(data.to_vec());
    run(&cmd).map(|_| ())
}

/// Already-rendered bytes (ZPL/ESC-POS) straight to the queue — CUPS must NOT re-render them.
pub fn raw_to_printer_cups(printer: &str, data: &[u8], run: &Runner) -> SendResult {
    lp(printer, &["-o", "raw"], Vec::new(), data, run)
}

/// CUPS renders PDF through its own filter chain (gotcha #1: never send PDF as raw).
pub fn pdf_to_printer_cups(
    printer: &str,
    data: &[u8],
    options: Option<&Options>,
    run: &Runner,
) -> SendResult {
    let extra = match options.filter(|o| !o.is_empty()) {
        Some(o) => lp_options(o)?,
        None => Vec::new(),
    };
    lp(printer, &[], extra, data, run)
}

/// Parse `lpoptions -p <queue> -l` output -> capabilities, or `None` if nothing useful.
pub fn caps_from_lpoptions(text: &str) -> Option<Capabilities> {
    let mut caps = Capabilities::default();
    for line in text.lines() {
        let Some((head, choices)) = line.split_once(':') else {
            continue;
        };
        let key = head.split('/').next().unwrap_or("").trim();
        let vals: Vec<String> = choices
            .split_whitespace()
            .map(|c| c.trim_start_matches('*').to_string())
            .collect();
        match key {
            "PageSize" => caps.papers = Some(vals),
            "InputSlot" => caps.bins = Some(vals),
            "Duplex" => caps.duplex = Some(vals.iter().any(|v| v != "None")),
            "ColorModel" => {
                caps.color = Some(vals.iter().any(|v| {
                    let v = v.to_lowercase();
                    v != "gray" && v != "grayscale"
                }))
            }
            _ => {}
        }
    }
    (!caps.is_empty()).then_some(caps)
}

/// Best-effort: a missing tool or a driver quirk must never block registration.
pub fn collect_capabilities_cups(printer: &str, run: &Runner) -> Option<Capabilities> {
    let mut cmd = Cmd::new(
        vec!["lpoptions".into(), "-p".into(), printer.into(), "-l".into()],
        10,
    );
    cmd.capture = true;
    let out = run(&cmd).ok()?;
    caps_from_lpoptions(&String::from_utf8_lossy(&out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::tests::obj;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn recording() -> (Runner, Rc<RefCell<Vec<Cmd>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let c = calls.clone();
        let run: Runner = Rc::new(move |cmd| {
            c.borrow_mut().push(cmd.clone());
            Ok(Vec::new())
        });
        (run, calls)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn test_cups_pdf_maps_options_to_lp_o() {
        let (run, calls) = recording();
        let opts = obj(
            json!({"duplex": "long-edge", "paper": "A4", "bin": "Tray1", "color": true, "pages": "1-2"}),
        );
        pdf_to_printer_cups("HP", b"%PDF", Some(&opts), &run).unwrap();
        let argv = &calls.borrow()[0].argv;
        assert_eq!(argv[..3], s(&["lp", "-d", "HP"])[..]);
        for o in [
            "sides=two-sided-long-edge",
            "media=A4",
            "InputSlot=Tray1",
            "print-color-mode=color",
            "page-ranges=1-2",
        ] {
            assert!(argv.contains(&o.to_string()), "{o}");
        }
    }

    #[test]
    fn test_cups_pdf_rejects_whitespace_option_values() {
        let (run, calls) = recording();
        let e = pdf_to_printer_cups("HP", b"%PDF", Some(&obj(json!({"paper": "A4 raw"}))), &run)
            .unwrap_err();
        assert!(e.contains("whitespace"));
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn test_cups_raw_pipes_data_with_raw_option() {
        let (run, calls) = recording();
        raw_to_printer_cups("Zebra", b"^XA^XZ", &run).unwrap();
        let c = &calls.borrow()[0];
        assert_eq!(c.argv, s(&["lp", "-d", "Zebra", "-o", "raw"]));
        assert_eq!(c.input.as_deref(), Some(&b"^XA^XZ"[..]));
        assert_eq!(c.timeout, std::time::Duration::from_secs(60));
    }

    #[test]
    fn test_cups_pdf_pipes_data_without_raw_option() {
        let (run, calls) = recording();
        pdf_to_printer_cups("HP", b"%PDF-1.4", None, &run).unwrap();
        let c = &calls.borrow()[0];
        assert_eq!(c.argv, s(&["lp", "-d", "HP"])); // CUPS renders PDF itself; no -o raw
        assert_eq!(c.input.as_deref(), Some(&b"%PDF-1.4"[..]));
    }

    #[test]
    fn a_failing_lp_surfaces_its_error() {
        let run: Runner =
            Rc::new(|_| Err("Command '['lp']' returned non-zero exit status 1.".into()));
        assert!(
            raw_to_printer_cups("Z", b"x", &run)
                .unwrap_err()
                .contains("non-zero")
        );
    }

    #[test]
    fn test_caps_from_lpoptions_parses_pagesize_inputslot_duplex_color() {
        let text = "PageSize/Media Size: *A4 Letter Legal Custom.WIDTHxHEIGHT\n\
                    InputSlot/Media Source: *Tray1 Tray2 Manual\n\
                    Duplex/2-Sided Printing: *None DuplexNoTumble DuplexTumble\n\
                    ColorModel/Color Model: *Gray RGB\n";
        assert_eq!(
            caps_from_lpoptions(text),
            Some(Capabilities {
                papers: Some(s(&["A4", "Letter", "Legal", "Custom.WIDTHxHEIGHT"])),
                bins: Some(s(&["Tray1", "Tray2", "Manual"])),
                duplex: Some(true),
                color: Some(true),
            })
        );
    }

    #[test]
    fn test_caps_from_lpoptions_mono_no_duplex_and_empty() {
        let text = "Duplex/2-Sided Printing: *None\nColorModel/Output Mode: *Gray\n";
        assert_eq!(
            caps_from_lpoptions(text),
            Some(Capabilities {
                duplex: Some(false),
                color: Some(false),
                ..Default::default()
            })
        );
        assert_eq!(caps_from_lpoptions(""), None);
    }

    #[test]
    fn test_collect_capabilities_cups_runs_lpoptions_and_survives_errors() {
        let seen = Rc::new(RefCell::new(None));
        let sn = seen.clone();
        let run: Runner = Rc::new(move |cmd| {
            *sn.borrow_mut() = Some(cmd.clone());
            Ok(b"PageSize/Media Size: *A4\n".to_vec())
        });
        let caps = collect_capabilities_cups("HP", &run).unwrap();
        assert_eq!(caps.papers, Some(s(&["A4"])));
        let c = seen.borrow().clone().unwrap();
        assert_eq!(c.argv, s(&["lpoptions", "-p", "HP", "-l"]));
        assert!(c.capture);
        assert_eq!(c.timeout, std::time::Duration::from_secs(10));

        let boom: Runner = Rc::new(|_| Err("no lpoptions".into()));
        assert_eq!(collect_capabilities_cups("HP", &boom), None);
    }
}

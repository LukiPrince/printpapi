// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Windows: RAW jobs through the spooler (winspool), PDFs through SumatraPDF, capabilities from
//! `DeviceCapabilitiesW`. The command-line and capability logic is portable (and tested everywhere);
//! only the winspool calls in `win32` are `cfg(windows)`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::Capabilities;
use crate::print::{Options, SendResult, sumatra_settings};
use crate::proc::{Cmd, Runner};

/// `DC_PAPERNAMES`, `DC_BINNAMES`, `DC_DUPLEX`, `DC_COLORDEVICE` and the entry widths (in WCHARs).
pub const DC_PAPERNAMES: u16 = 16;
pub const DC_BINNAMES: u16 = 12;
pub const DC_DUPLEX: u16 = 7;
pub const DC_COLORDEVICE: u16 = 32;
pub const PAPER_NAME_LEN: usize = 64;
pub const BIN_NAME_LEN: usize = 24;

/// The winspool calls capability discovery needs; the real one is `Win32`, tests fake it.
pub trait WinPrint {
    /// The printer's port name (`GetPrinter` level 2, `pPortName`).
    fn port_name(&self, printer: &str) -> Result<String, String>;
    /// A `DeviceCapabilities` call that fills fixed-width name entries.
    fn names(&self, printer: &str, port: &str, cap: u16) -> Result<Vec<String>, String>;
    /// A `DeviceCapabilities` call whose return value is the answer.
    fn number(&self, printer: &str, port: &str, cap: u16) -> Result<i32, String>;
}

/// Best-effort: a driver quirk must never block registration, so ANY failure is `None`.
pub fn collect_capabilities_windows(printer: &str, wp: &dyn WinPrint) -> Option<Capabilities> {
    let port = wp.port_name(printer).ok()?;
    let clean = |v: Vec<String>| {
        v.into_iter()
            .map(|s| s.trim_matches(|c| c == '\0' || c == ' ').to_string())
            .collect()
    };
    let papers = wp.names(printer, &port, DC_PAPERNAMES).ok()?;
    let bins = wp.names(printer, &port, DC_BINNAMES).ok()?;
    let duplex = wp.number(printer, &port, DC_DUPLEX).ok()?;
    let color = wp.number(printer, &port, DC_COLORDEVICE).ok()?;
    Some(Capabilities {
        papers: Some(clean(papers)),
        bins: Some(clean(bins)),
        duplex: Some(duplex != 0),
        color: Some(color != 0),
    })
}

/// A temp file removed on drop. Created exclusively under an unpredictable name (the `mkstemp`
/// of Python's original).
struct TempPdf(PathBuf);

impl TempPdf {
    fn create(data: &[u8]) -> Result<TempPdf, String> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        for _ in 0..16 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos() as u64 ^ d.as_secs().rotate_left(20));
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "printpapi-{}-{nanos:x}-{n}.pdf",
                std::process::id()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    let tmp = TempPdf(path); // from here on, dropping removes it
                    f.write_all(data).map_err(|e| e.to_string())?;
                    return Ok(tmp);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("could not create a temporary file".into())
    }
}

impl Drop for TempPdf {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `<sumatra> -print-to <printer> [-print-settings <s>] -silent <tmp.pdf>`, 60 s, temp file always removed.
pub fn pdf_to_printer(
    printer: &str,
    data: &[u8],
    options: Option<&Options>,
    sumatra: &str,
    run: &Runner,
) -> SendResult {
    let tmp = TempPdf::create(data)?;
    let mut argv = vec![
        sumatra.to_string(),
        "-print-to".to_string(),
        printer.to_string(),
    ];
    if let Some(o) = options.filter(|o| !o.is_empty()) {
        let settings = sumatra_settings(o)?;
        if !settings.is_empty() {
            argv.push("-print-settings".into());
            argv.push(settings);
        }
    }
    argv.push("-silent".into());
    argv.push(tmp.0.to_string_lossy().into_owned());
    run(&Cmd::new(argv, 60)).map(|_| ())
}

#[cfg(not(windows))]
pub fn raw_to_printer(_printer: &str, _data: &[u8]) -> SendResult {
    Err("RAW printing through the Windows spooler is only available on Windows".into())
}

#[cfg(windows)]
pub use win32::{Win32, raw_to_printer};

#[cfg(windows)]
mod win32 {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, DOC_INFO_1W, EndDocPrinter, EndPagePrinter, GetPrinterW, OpenPrinterW,
        PRINTER_HANDLE, PRINTER_INFO_2W, StartDocPrinterW, StartPagePrinter, WritePrinter,
    };
    use windows_sys::Win32::Storage::Xps::DeviceCapabilitiesW;

    use super::WinPrint;
    use crate::print::SendResult;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// `GetLastError()` as a readable message.
    fn last_error(call: &str) -> String {
        format!("{call} failed: {}", std::io::Error::last_os_error())
    }

    /// An open printer handle, closed on drop (the `finally: ClosePrinter` of the original).
    struct Handle(PRINTER_HANDLE);

    impl Handle {
        fn open(name: &str) -> Result<Handle, String> {
            let name = wide(name);
            let mut h = PRINTER_HANDLE { Value: null_mut() };
            // SAFETY: `name` is NUL-terminated and outlives the call; `h` is a valid out-pointer.
            if unsafe { OpenPrinterW(name.as_ptr(), &mut h, null()) } == 0 {
                return Err(last_error("OpenPrinter"));
            }
            Ok(Handle(h))
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful OpenPrinterW and is closed exactly once.
            unsafe { ClosePrinter(self.0) };
        }
    }

    /// RAW datatype job: StartDoc, StartPage, WritePrinter until every byte is written, EndPage,
    /// EndDoc. The document is ended and the handle closed on every path.
    pub fn raw_to_printer(printer: &str, data: &[u8]) -> SendResult {
        let h = Handle::open(printer)?;
        let mut doc_name = wide("print-agent");
        let mut datatype = wide("RAW");
        let info = DOC_INFO_1W {
            pDocName: doc_name.as_mut_ptr(),
            pOutputFile: null_mut(),
            pDatatype: datatype.as_mut_ptr(),
        };
        // SAFETY: `info` points at NUL-terminated buffers that live until after the call.
        if unsafe { StartDocPrinterW(h.0, 1, &info) } == 0 {
            return Err(last_error("StartDocPrinter"));
        }
        let result = write_page(&h, data);
        // SAFETY: the document was started above; ending it after a failed page is the cleanup.
        let ended = unsafe { EndDocPrinter(h.0) };
        result?;
        if ended == 0 {
            return Err(last_error("EndDocPrinter"));
        }
        Ok(())
    }

    fn write_page(h: &Handle, data: &[u8]) -> SendResult {
        // SAFETY: plain handle calls on an open printer with a started document.
        if unsafe { StartPagePrinter(h.0) } == 0 {
            return Err(last_error("StartPagePrinter"));
        }
        let mut rest = data;
        while !rest.is_empty() {
            let chunk = rest.len().min(1 << 20);
            let mut written = 0u32;
            // SAFETY: `rest` is valid for `chunk` bytes; `written` is a valid out-pointer.
            if unsafe { WritePrinter(h.0, rest.as_ptr().cast(), chunk as u32, &mut written) } == 0 {
                return Err(last_error("WritePrinter"));
            }
            if written == 0 {
                return Err("WritePrinter wrote 0 bytes".into());
            }
            rest = &rest[(written as usize).min(rest.len())..];
        }
        // SAFETY: as above; the page was started.
        if unsafe { EndPagePrinter(h.0) } == 0 {
            return Err(last_error("EndPagePrinter"));
        }
        Ok(())
    }

    /// The real winspool-backed [`WinPrint`].
    pub struct Win32;

    /// A NUL-terminated wide string that winspool placed inside `buf`. Read only within `buf`:
    /// a pointer outside it, or a string without a terminator before its end, reads as "".
    fn read_wstr(p: *const u16, buf: &[u64]) -> String {
        let start = buf.as_ptr() as usize;
        let end = start + std::mem::size_of_val(buf);
        let at = p as usize;
        if p.is_null() || at < start || at >= end || !(at - start).is_multiple_of(2) {
            return String::new();
        }
        // SAFETY: `p` lies inside `buf`, is u16-aligned (buf is u64-aligned and the offset is
        // even), and the slice ends at the end of `buf`.
        let units = unsafe { std::slice::from_raw_parts(p, (end - at) / 2) };
        match units.iter().position(|&u| u == 0) {
            Some(len) => String::from_utf16_lossy(&units[..len]),
            None => String::new(),
        }
    }

    impl WinPrint for Win32 {
        fn port_name(&self, printer: &str) -> Result<String, String> {
            let h = Handle::open(printer)?;
            let mut needed = 0u32;
            // SAFETY: a size query with a null buffer; `needed` is a valid out-pointer.
            unsafe { GetPrinterW(h.0, 2, null_mut(), 0, &mut needed) };
            if needed == 0 || needed > 1 << 20 {
                return Err(last_error("GetPrinter"));
            }
            // u64 words keep the buffer aligned for PRINTER_INFO_2W's pointers.
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            // SAFETY: `buf` spans at least `needed` bytes.
            if unsafe { GetPrinterW(h.0, 2, buf.as_mut_ptr().cast(), needed, &mut needed) } == 0 {
                return Err(last_error("GetPrinter"));
            }
            // SAFETY: a successful level-2 call filled the buffer with a PRINTER_INFO_2W whose
            // string pointers point into the same buffer.
            let info = unsafe { &*(buf.as_ptr() as *const PRINTER_INFO_2W) };
            Ok(read_wstr(info.pPortName, &buf))
        }

        fn names(&self, printer: &str, port: &str, cap: u16) -> Result<Vec<String>, String> {
            let width = match cap {
                super::DC_PAPERNAMES => super::PAPER_NAME_LEN,
                _ => super::BIN_NAME_LEN,
            };
            let (dev, port) = (wide(printer), wide(port));
            // SAFETY: both names are NUL-terminated; a null output asks for the entry count.
            let count = unsafe {
                DeviceCapabilitiesW(dev.as_ptr(), port.as_ptr(), cap, null_mut(), null())
            };
            if count < 0 {
                return Err(last_error("DeviceCapabilities"));
            }
            // The fill call has no capacity argument: it writes as many entries as it reports, so
            // a buffer for fewer would be overrun. Refuse an implausible count instead.
            let count = count as usize;
            if count > 10_000 {
                return Err(format!("DeviceCapabilities reported {count} entries"));
            }
            let mut buf = vec![0u16; count * width];
            if count > 0 {
                // SAFETY: `buf` holds `count` entries of `width` WCHARs, what the capability fills.
                let got = unsafe {
                    DeviceCapabilitiesW(dev.as_ptr(), port.as_ptr(), cap, buf.as_mut_ptr(), null())
                };
                if got < 0 {
                    return Err(last_error("DeviceCapabilities"));
                }
            }
            Ok(buf
                .chunks(width)
                .map(|c| {
                    String::from_utf16_lossy(
                        &c[..c.iter().position(|&u| u == 0).unwrap_or(c.len())],
                    )
                })
                .collect())
        }

        fn number(&self, printer: &str, port: &str, cap: u16) -> Result<i32, String> {
            let (dev, port) = (wide(printer), wide(port));
            // SAFETY: both names are NUL-terminated; these capabilities need no output buffer.
            let v = unsafe {
                DeviceCapabilitiesW(dev.as_ptr(), port.as_ptr(), cap, null_mut(), null())
            };
            if v < 0 {
                Err(last_error("DeviceCapabilities"))
            } else {
                Ok(v)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::tests::obj;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct FakeWP {
        fail_open: bool,
    }

    impl WinPrint for FakeWP {
        fn port_name(&self, _: &str) -> Result<String, String> {
            if self.fail_open {
                Err("no driver".into())
            } else {
                Ok("USB001".into())
            }
        }
        fn names(&self, _: &str, port: &str, cap: u16) -> Result<Vec<String>, String> {
            assert_eq!(port, "USB001");
            Ok(match cap {
                16 => vec!["A4\0".into(), "Letter".into()],
                12 => vec!["Tray 1".into()],
                _ => unreachable!(),
            })
        }
        fn number(&self, _: &str, _: &str, cap: u16) -> Result<i32, String> {
            Ok(match cap {
                7 => 1,
                32 => 0,
                _ => unreachable!(),
            })
        }
    }

    #[test]
    fn test_collect_capabilities_windows_via_fake_win32print() {
        assert_eq!(
            collect_capabilities_windows("HP", &FakeWP { fail_open: false }),
            Some(Capabilities {
                papers: Some(vec!["A4".into(), "Letter".into()]),
                bins: Some(vec!["Tray 1".into()]),
                duplex: Some(true),
                color: Some(false),
            })
        );
    }

    #[test]
    fn test_collect_capabilities_windows_errors_return_none() {
        assert_eq!(
            collect_capabilities_windows("HP", &FakeWP { fail_open: true }),
            None
        );
    }

    fn recording() -> (Runner, Rc<RefCell<Vec<Cmd>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let c = calls.clone();
        let run: Runner = Rc::new(move |cmd| {
            // the temp file exists while Sumatra runs, and holds the PDF
            let path = cmd.argv.last().unwrap();
            assert_eq!(std::fs::read(path).unwrap(), b"%PDF");
            c.borrow_mut().push(cmd.clone());
            Ok(Vec::new())
        });
        (run, calls)
    }

    #[test]
    fn test_pdf_to_printer_passes_print_settings() {
        let (run, calls) = recording();
        pdf_to_printer(
            "HP",
            b"%PDF",
            Some(&obj(json!({"duplex": "short-edge"}))),
            "SumatraPDF.exe",
            &run,
        )
        .unwrap();
        let c = &calls.borrow()[0];
        assert_eq!(c.argv[..3], ["SumatraPDF.exe", "-print-to", "HP"]);
        let i = c.argv.iter().position(|a| a == "-print-settings").unwrap();
        assert_eq!(c.argv[i + 1], "duplexshort");
        assert_eq!(c.argv[c.argv.len() - 2], "-silent");
        assert!(c.argv.last().unwrap().ends_with(".pdf"));
        assert_eq!(c.timeout, std::time::Duration::from_secs(60));
    }

    #[test]
    fn test_pdf_to_printer_orientation_auto_no_print_settings() {
        let (run, calls) = recording();
        let opts = obj(json!({"orientation": "auto"}));
        pdf_to_printer("HP", b"%PDF", Some(&opts), "SumatraPDF.exe", &run).unwrap();
        assert!(
            !calls.borrow()[0]
                .argv
                .contains(&"-print-settings".to_string())
        );
    }

    #[test]
    fn test_pdf_to_printer_no_options_no_print_settings() {
        let (run, calls) = recording();
        pdf_to_printer("HP", b"%PDF", None, "SumatraPDF.exe", &run).unwrap();
        assert!(
            !calls.borrow()[0]
                .argv
                .contains(&"-print-settings".to_string())
        );
    }

    #[test]
    fn test_select_backend_windows_pdf_forwards_options() {
        let (run, calls) = recording();
        let (_, pdf_w) = crate::select_backend(crate::Platform::Windows, "S.exe", run);
        pdf_w("HP", b"%PDF", Some(&obj(json!({"paper": "A4"})))).unwrap();
        let c = &calls.borrow()[0];
        assert_eq!(c.argv[0], "S.exe");
        assert!(c.argv.contains(&"paper=A4".to_string()));
    }

    #[test]
    fn the_temp_file_is_removed_even_when_sumatra_fails() {
        let path = Rc::new(RefCell::new(String::new()));
        let p = path.clone();
        let run: Runner = Rc::new(move |cmd| {
            *p.borrow_mut() = cmd.argv.last().unwrap().clone();
            Err("Command '[...]' returned non-zero exit status 1.".into())
        });
        assert!(pdf_to_printer("HP", b"%PDF", None, "S.exe", &run).is_err());
        assert!(!std::path::Path::new(&*path.borrow()).exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn raw_spooler_printing_is_refused_off_windows() {
        assert!(raw_to_printer("P", b"x").is_err());
    }

    /// Real FFI against an installed printer; never prints. Run with `--ignored`.
    #[cfg(windows)]
    #[test]
    #[ignore = "needs an installed printer (Microsoft Print to PDF)"]
    fn real_capabilities_for_microsoft_print_to_pdf() {
        let caps =
            collect_capabilities_windows("Microsoft Print to PDF", &Win32).expect("capabilities");
        eprintln!("{caps:?}");
        assert!(caps.papers.as_ref().is_some_and(|p| !p.is_empty()));
        assert!(caps.bins.is_some() && caps.duplex.is_some() && caps.color.is_some());
        assert!(collect_capabilities_windows("No Such Printer 123", &Win32).is_none());
    }
}

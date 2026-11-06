// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! Getting one job's bytes onto paper (or a socket, or a directory): option mapping, the file and
//! socket targets, and the dispatch in [`print_job`]. The OS backends live in `backend_*`.

use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::config::{Capabilities, Printer, file_target_dir};
use crate::pyrepr::{py_str, repr_str, truthy};

/// A job's `options` (duplex/paper/bin/color/pages/orientation, validated by the server).
pub type Options = Map<String, Value>;
pub type SendResult = Result<(), String>;
pub type RawFn = Box<dyn Fn(&str, &[u8]) -> SendResult>;
pub type PdfFn = Box<dyn Fn(&str, &[u8], Option<&Options>) -> SendResult>;

/// What `print_job` sends through. Production builds it from [`crate::select_backend`] plus the
/// socket and file senders; tests swap any of the four.
pub struct Fns {
    pub raw: RawFn,
    pub pdf: PdfFn,
    pub socket: RawFn,
    pub file: Box<dyn Fn(&FileJob) -> SendResult>,
}

/// One copy of a job headed for a `file://` printer.
pub struct FileJob<'a> {
    pub target: &'a str,
    pub data: &'a [u8],
    pub mode: &'a str,
    pub job_id: Option<&'a str>,
    pub index: u32,
}

impl Fns {
    /// Every sender fails with "not expected" — a base for tests that must not touch anything real.
    pub fn refusing() -> Fns {
        Fns {
            raw: Box::new(|_, _| Err("raw not expected".into())),
            pdf: Box::new(|_, _, _| Err("pdf not expected".into())),
            socket: Box::new(|_, _| Err("socket not expected".into())),
            file: Box::new(|_| Err("file not expected".into())),
        }
    }

    /// The real senders: the OS backend for raw/pdf, TCP for `socket://`, disk for `file://`.
    pub fn native(raw: RawFn, pdf: PdfFn) -> Fns {
        Fns {
            raw,
            pdf,
            socket: Box::new(raw_to_socket),
            file: Box::new(|j| {
                write_to_file(j.target, j.data, j.mode, j.job_id, j.index).map(|_| ())
            }),
        }
    }
}

const SUMATRA_DUPLEX: [(&str, &str); 3] = [
    ("long-edge", "duplexlong"),
    ("short-edge", "duplexshort"),
    ("one-sided", "simplex"),
];
const CUPS_DUPLEX: [(&str, &str); 3] = [
    ("long-edge", "two-sided-long-edge"),
    ("short-edge", "two-sided-short-edge"),
    ("one-sided", "one-sided"),
];

fn opt_str(options: &Options, key: &str) -> Result<String, String> {
    match &options[key] {
        Value::String(s) => Ok(s.clone()),
        other => Err(format!(
            "option {key} must be a string, got {}",
            py_str(other)
        )),
    }
}

fn map_duplex(options: &Options, table: &[(&str, &'static str)]) -> Result<&'static str, String> {
    let v = opt_str(options, "duplex")?;
    table
        .iter()
        .find(|(k, _)| *k == v)
        .map(|(_, t)| *t)
        .ok_or_else(|| format!("unknown duplex value {}", repr_str(&v)))
}

/// options dict -> SumatraPDF `-print-settings` value (comma-separated list).
pub fn sumatra_settings(options: &Options) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    if options.contains_key("pages") {
        parts.push(opt_str(options, "pages")?);
    }
    if options.contains_key("duplex") {
        parts.push(map_duplex(options, &SUMATRA_DUPLEX)?.to_string());
    }
    if options.contains_key("paper") {
        parts.push(format!("paper={}", opt_str(options, "paper")?));
    }
    if options.contains_key("bin") {
        parts.push(format!("bin={}", opt_str(options, "bin")?));
    }
    if let Some(c) = options.get("color") {
        parts.push(if truthy(c) { "color" } else { "monochrome" }.to_string());
    }
    // Sumatra's own two words, nothing more: `disable-auto-rotation` next to `landscape` turns a
    // wide page 90 degrees, and before 3.5 it is ignored, which leaves `portrait` turning that page
    // 270 (measured on 3.4.6, 3.5.2, 3.6.1; the rule is in Sumatra's src/Print.cpp).
    if options.contains_key("orientation") {
        let v = opt_str(options, "orientation")?;
        match v.as_str() {
            "auto" => {}
            "portrait" | "landscape" => parts.push(v),
            _ => return Err(format!("unknown orientation value {}", repr_str(&v))),
        }
    }
    Ok(parts.join(","))
}

/// options dict -> `["-o", "k=v", ...]` for lp. lp splits one `-o` value on spaces, so a value
/// with whitespace could smuggle extra options (e.g. `raw`) in — refuse those.
///
/// `orientation` stays out: CUPS reads the real paper size and turns a page only when the two
/// disagree. `orientation-requested=3` changed nothing there and `=4` printed upside down
/// (cups-filters 1.28.15).
pub fn lp_options(options: &Options) -> Result<Vec<String>, String> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if options.contains_key("duplex") {
        pairs.push(("sides", map_duplex(options, &CUPS_DUPLEX)?.to_string()));
    }
    if options.contains_key("paper") {
        pairs.push(("media", opt_str(options, "paper")?));
    }
    if options.contains_key("bin") {
        pairs.push(("InputSlot", opt_str(options, "bin")?));
    }
    if let Some(c) = options.get("color") {
        pairs.push((
            "print-color-mode",
            if truthy(c) { "color" } else { "monochrome" }.to_string(),
        ));
    }
    if options.contains_key("pages") {
        pairs.push(("page-ranges", opt_str(options, "pages")?));
    }
    let mut out = Vec::new();
    for (k, v) in pairs {
        if v.chars().any(char::is_whitespace) {
            return Err(format!(
                "option {k} value must not contain whitespace: {}",
                repr_str(&v)
            ));
        }
        out.push("-o".to_string());
        out.push(format!("{k}={v}"));
    }
    Ok(out)
}

/// Raw bytes straight to a network printer's socket (e.g. Zebra :9100). Already-rendered only
/// (ZPL/ESC-POS) — gotcha #1: a bare socket has no renderer, so PDF must never reach here.
pub fn raw_to_socket(target: &str, data: &[u8]) -> SendResult {
    raw_to_socket_with(target, data, &|host, port, timeout| {
        let mut last = None;
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(s) => {
                    s.set_write_timeout(Some(timeout))?;
                    return Ok(Box::new(s) as Box<dyn Write>);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("no address")))
    })
}

type Connect<'a> = &'a dyn Fn(&str, u16, Duration) -> std::io::Result<Box<dyn Write>>;

/// [`raw_to_socket`] with the connection injectable (the tests' `connect=`).
pub fn raw_to_socket_with(target: &str, data: &[u8], connect: Connect) -> SendResult {
    let addr = target.strip_prefix("socket://").unwrap_or(target);
    // ponytail: IPv4 host:port; no IPv6-in-brackets support
    let (host, port) = addr.rsplit_once(':').unwrap_or(("", addr));
    let port: u16 = port
        .parse()
        .map_err(|_| format!("invalid port in socket target {}", repr_str(target)))?;
    let mut s = connect(host, port, Duration::from_secs(30)).map_err(|e| e.to_string())?;
    s.write_all(data)
        .and_then(|_| s.flush())
        .map_err(|e| e.to_string())
}

/// A job id as it may appear in a file name: digits (or a plain word) only, so a hostile server
/// cannot walk the write out of the target directory.
pub fn safe_job_id(job_id: &str) -> String {
    job_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// "Virtual print server": drop the job in a directory instead of on paper — archival and
/// paperless workflows. A pdf job's payload already *is* a PDF, so nothing renders here; raw
/// payloads (ZPL/ESC-POS) are written verbatim as `.prn`. Returns the path written.
pub fn write_to_file(
    target: &str,
    data: &[u8],
    mode: &str,
    job_id: Option<&str>,
    index: u32,
) -> Result<String, String> {
    let directory = file_target_dir(target)?;
    std::fs::create_dir_all(&directory).map_err(|e| format!("{e}: {}", repr_str(&directory)))?;
    let mut stem = job_id.map_or_else(
        || "job".to_string(),
        |id| format!("job-{}", safe_job_id(id)),
    );
    if index > 1 {
        stem.push_str(&format!("-{index}")); // copies of one job must not overwrite each other
    }
    let name = format!("{stem}{}", if mode == "pdf" { ".pdf" } else { ".prn" });
    let path = Path::new(&directory).join(&name);
    write_replacing(&path, &name, data)
        .map_err(|e| format!("{e}: {}", repr_str(&path.to_string_lossy())))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Write `data` to `path` via a fresh hidden `.partial` file in the same directory, renamed over
/// `path` when complete. A consume folder (Paperless, a NAS sync) never sees a half-written job,
/// and a symlink planted at `path` is replaced instead of followed (`create_new` also refuses a
/// link at the temporary name). The Python agent wrote `path` in place.
fn write_replacing(path: &Path, name: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.partial",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(data).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Attach discovered capabilities to `parse_printers()` entries. socket:// and file:// targets
/// have no driver/queue to ask; a `None` result (collector failed) just leaves the entry as-is.
pub fn add_capabilities(
    mut printers: Vec<Printer>,
    caps_fn: &dyn Fn(&str) -> Option<Capabilities>,
) -> Vec<Printer> {
    for p in &mut printers {
        if !p.target.starts_with("socket://") && !p.target.starts_with("file://") {
            p.capabilities = caps_fn(&p.target).filter(|c| !c.is_empty());
        }
    }
    printers
}

/// The most copies one job may ask for; the server itself caps at 100.
pub const MAX_COPIES: i64 = 1000;

pub fn print_job(
    mode: &str,
    entry: &Printer,
    data: &[u8],
    copies: i64,
    options: Option<&Options>,
    fns: &Fns,
    job_id: Option<&str>,
) -> SendResult {
    let target = entry.target.as_str();
    if mode != "raw" && mode != "pdf" {
        return Err(format!("bad mode: {mode}"));
    }
    if copies > MAX_COPIES {
        return Err(format!("copies must be at most {MAX_COPIES}, got {copies}"));
    }
    let options = options.filter(|o| !o.is_empty());
    // ponytail: loop the whole send per copy — correct on every backend without a per-driver
    // copies flag (win32/Sumatra/CUPS/socket differ). Native flags (lp -n, Sumatra "Nx") would
    // be one call instead of N; not worth the branching for the small copy counts labels use.
    let send: Box<dyn Fn(u32) -> SendResult + '_> = if target.starts_with("socket://") {
        if mode != "raw" {
            return Err("network socket printer is raw-only (cannot render PDF)".into());
        }
        Box::new(move |_| (fns.socket)(target, data))
    } else if target.starts_with("file://") {
        // options are print-hardware settings (tray/duplex/…) — nothing to apply to a file
        Box::new(move |i| {
            (fns.file)(&FileJob {
                target,
                data,
                mode,
                job_id,
                index: i,
            })
        })
    } else if mode == "raw" {
        Box::new(move |_| (fns.raw)(target, data))
    } else {
        Box::new(move |_| (fns.pdf)(target, data, options))
    };
    for i in 1..=copies.max(0) {
        send(i as u32)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub fn obj(v: Value) -> Options {
        v.as_object().cloned().unwrap()
    }

    pub fn entry(name: &str, can_pdf: bool, target: &str) -> Printer {
        Printer::new(name, can_pdf, target)
    }

    /// `file://` URI for a local directory, in the spelling `docs/agent.md` documents.
    pub fn file_uri(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "/");
        if s.starts_with('/') {
            format!("file://{s}")
        } else {
            format!("file:///{s}")
        }
    }

    #[test]
    fn test_print_job_copies_repeats_raw_send() {
        let sends = Rc::new(RefCell::new(Vec::new()));
        let s = sends.clone();
        let fns = Fns {
            raw: Box::new(move |_, d| {
                s.borrow_mut().push(d.to_vec());
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "raw",
            &entry("Zebra", false, "Zebra"),
            b"ZPL",
            3,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*sends.borrow(), vec![b"ZPL".to_vec(); 3]);
    }

    #[test]
    fn test_print_job_copies_repeats_pdf_and_socket() {
        let pdfs = Rc::new(RefCell::new(Vec::new()));
        let p = pdfs.clone();
        let fns = Fns {
            pdf: Box::new(move |_, d, _| {
                p.borrow_mut().push(d.to_vec());
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "pdf",
            &entry("HP", true, "HP"),
            b"%PDF",
            2,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*pdfs.borrow(), vec![b"%PDF".to_vec(); 2]);

        let socks = Rc::new(RefCell::new(Vec::new()));
        let s = socks.clone();
        let fns = Fns {
            socket: Box::new(move |_, d| {
                s.borrow_mut().push(d.to_vec());
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "raw",
            &entry("n", false, "socket://10.0.0.5:9100"),
            b"Z",
            2,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*socks.borrow(), vec![b"Z".to_vec(); 2]);
    }

    #[test]
    fn a_failing_copy_stops_the_loop() {
        let n = Rc::new(RefCell::new(0));
        let c = n.clone();
        let fns = Fns {
            raw: Box::new(move |_, _| {
                *c.borrow_mut() += 1;
                Err("spooler down".into())
            }),
            ..Fns::refusing()
        };
        let e = print_job("raw", &entry("Z", false, "Z"), b"x", 3, None, &fns, None).unwrap_err();
        assert_eq!(e, "spooler down");
        assert_eq!(*n.borrow(), 1);
    }

    #[test]
    fn copies_below_one_print_nothing_and_above_the_cap_are_refused() {
        let fns = Fns::refusing();
        assert!(print_job("raw", &entry("Z", false, "Z"), b"x", 0, None, &fns, None).is_ok());
        assert!(
            print_job(
                "raw",
                &entry("Z", false, "Z"),
                b"x",
                MAX_COPIES + 1,
                None,
                &fns,
                None
            )
            .is_err()
        );
    }

    #[test]
    fn test_sumatra_settings_maps_all_options() {
        let s = sumatra_settings(&obj(json!({"duplex": "long-edge", "paper": "A4", "bin": "Tray 1", "color": false, "pages": "1-3,5"}))).unwrap();
        assert_eq!(s, "1-3,5,duplexlong,paper=A4,bin=Tray 1,monochrome");
        assert_eq!(
            sumatra_settings(&obj(json!({"duplex": "short-edge"}))).unwrap(),
            "duplexshort"
        );
        assert_eq!(
            sumatra_settings(&obj(json!({"duplex": "one-sided", "color": true}))).unwrap(),
            "simplex,color"
        );
    }

    #[test]
    fn orientation_is_sumatras_own_flag() {
        for v in ["portrait", "landscape"] {
            assert_eq!(
                sumatra_settings(&obj(json!({"orientation": v}))).unwrap(),
                v
            );
        }
        assert_eq!(
            sumatra_settings(&obj(
                json!({"duplex": "short-edge", "orientation": "landscape"})
            ))
            .unwrap(),
            "duplexshort,landscape"
        );
    }

    #[test]
    fn orientation_auto_adds_no_sumatra_flag() {
        assert_eq!(
            sumatra_settings(&obj(json!({"orientation": "auto"}))).unwrap(),
            ""
        );
    }

    #[test]
    fn unknown_orientation_is_an_error_not_a_flag() {
        assert_eq!(
            sumatra_settings(&obj(json!({"orientation": "sideways"}))).unwrap_err(),
            "unknown orientation value 'sideways'"
        );
        assert!(sumatra_settings(&obj(json!({"orientation": 90}))).is_err());
    }

    // orientation-requested=4 turned pages that already fit the paper upside down, so nothing of
    // `orientation` may reach lp.
    #[test]
    fn cups_gets_no_orientation_flag() {
        for v in ["auto", "portrait", "landscape"] {
            assert!(
                lp_options(&obj(json!({"orientation": v})))
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn option_values_of_the_wrong_type_or_unknown_duplex_are_errors_not_panics() {
        assert!(sumatra_settings(&obj(json!({"pages": 5}))).is_err());
        assert!(
            sumatra_settings(&obj(json!({"duplex": "sideways"})))
                .unwrap_err()
                .contains("unknown duplex")
        );
        assert!(lp_options(&obj(json!({"paper": ["A4"]}))).is_err());
    }

    #[test]
    fn test_cups_pdf_rejects_whitespace_option_values() {
        let e = lp_options(&obj(json!({"paper": "A4 raw"}))).unwrap_err();
        assert!(e.contains("whitespace"));
        assert_eq!(
            e,
            "option media value must not contain whitespace: 'A4 raw'"
        );
    }

    #[test]
    fn test_print_job_passes_options_to_pdf_fn() {
        let seen = Rc::new(RefCell::new(None));
        let s = seen.clone();
        let fns = Fns {
            pdf: Box::new(move |t, _, o| {
                *s.borrow_mut() = Some((t.to_string(), o.cloned()));
                Ok(())
            }),
            ..Fns::refusing()
        };
        let opts = obj(json!({"duplex": "long-edge"}));
        print_job(
            "pdf",
            &entry("HP", true, "HP"),
            b"%PDF",
            1,
            Some(&opts),
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*seen.borrow(), Some(("HP".to_string(), Some(opts))));
    }

    #[test]
    fn test_print_job_without_options_calls_pdf_fn_without_options() {
        let seen = Rc::new(RefCell::new(None));
        let s = seen.clone();
        let fns = Fns {
            pdf: Box::new(move |t, _, o| {
                *s.borrow_mut() = Some((t.to_string(), o.is_none()));
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "pdf",
            &entry("HP", true, "HP"),
            b"%PDF",
            1,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*seen.borrow(), Some(("HP".to_string(), true)));
        // an empty options object is "no options" too
        let empty = obj(json!({}));
        print_job(
            "pdf",
            &entry("HP", true, "HP"),
            b"%PDF",
            1,
            Some(&empty),
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*seen.borrow(), Some(("HP".to_string(), true)));
    }

    #[test]
    fn test_print_job_bad_mode_raises() {
        let e = print_job(
            "docx",
            &entry("P", false, "P"),
            b"x",
            1,
            None,
            &Fns::refusing(),
            None,
        )
        .unwrap_err();
        assert!(e.contains("bad mode"));
        assert_eq!(e, "bad mode: docx");
    }

    #[test]
    fn test_print_job_socket_raw_routes_to_socket_fn() {
        let seen = Rc::new(RefCell::new(None));
        let s = seen.clone();
        let fns = Fns {
            socket: Box::new(move |t, d| {
                *s.borrow_mut() = Some((t.to_string(), d.to_vec()));
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "raw",
            &entry("netz", false, "socket://10.0.0.5:9100"),
            b"Z",
            1,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(
            *seen.borrow(),
            Some(("socket://10.0.0.5:9100".to_string(), b"Z".to_vec()))
        );
    }

    #[test]
    fn test_print_job_socket_pdf_is_refused() {
        let e = print_job(
            "pdf",
            &entry("netz", false, "socket://10.0.0.5:9100"),
            b"%PDF",
            1,
            None,
            &Fns::refusing(),
            None,
        )
        .unwrap_err();
        assert!(e.contains("raw-only"));
    }

    #[test]
    fn test_print_job_local_raw_uses_target_name() {
        let seen = Rc::new(RefCell::new(None));
        let s = seen.clone();
        let fns = Fns {
            raw: Box::new(move |t, d| {
                *s.borrow_mut() = Some((t.to_string(), d.to_vec()));
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "raw",
            &entry("Zebra", false, "Zebra"),
            b"x",
            1,
            None,
            &fns,
            None,
        )
        .unwrap();
        assert_eq!(*seen.borrow(), Some(("Zebra".to_string(), b"x".to_vec())));
    }

    #[test]
    fn test_raw_to_socket_parses_addr_and_sends_bytes() {
        struct Sink(Rc<RefCell<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sent = Rc::new(RefCell::new(Vec::new()));
        let addr = Rc::new(RefCell::new(None));
        let (s, a) = (sent.clone(), addr.clone());
        raw_to_socket_with("socket://10.0.0.5:9100", b"^XA^XZ", &move |h, p, t| {
            *a.borrow_mut() = Some((h.to_string(), p, t));
            Ok(Box::new(Sink(s.clone())))
        })
        .unwrap();
        assert_eq!(
            *addr.borrow(),
            Some(("10.0.0.5".to_string(), 9100, Duration::from_secs(30)))
        );
        assert_eq!(*sent.borrow(), b"^XA^XZ");
    }

    #[test]
    fn raw_to_socket_really_connects_over_loopback() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            std::io::Read::read_to_end(&mut s, &mut got).unwrap();
            got
        });
        raw_to_socket(&format!("socket://127.0.0.1:{port}"), b"^XA^XZ").unwrap();
        assert_eq!(t.join().unwrap(), b"^XA^XZ");
    }

    #[test]
    fn raw_to_socket_bad_port_or_refused_connection_is_an_error() {
        assert!(
            raw_to_socket("socket://h:99999", b"x")
                .unwrap_err()
                .contains("invalid port")
        );
        assert!(raw_to_socket("socket://127.0.0.1:1", b"x").is_err());
    }

    #[test]
    fn test_write_to_file_writes_pdf_named_after_the_job() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_uri(&dir.path().join("inbox")); // dir does not exist yet
        let path = write_to_file(&target, b"%PDF-1.4", "pdf", Some("42"), 1).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("inbox").join("job-42.pdf")).unwrap(),
            b"%PDF-1.4"
        );
        assert!(path.ends_with("job-42.pdf"));
    }

    #[test]
    fn test_write_to_file_raw_uses_prn_and_copies_do_not_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_uri(dir.path());
        write_to_file(&target, b"^XA^XZ", "raw", Some("7"), 1).unwrap();
        write_to_file(&target, b"^XA^XZ", "raw", Some("7"), 2).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("job-7.prn")).unwrap(),
            b"^XA^XZ"
        );
        assert!(dir.path().join("job-7-2.prn").exists());
        write_to_file(&target, b"x", "raw", None, 1).unwrap();
        assert!(dir.path().join("job.prn").exists());
    }

    #[test]
    fn a_rewritten_job_replaces_the_file_and_leaves_no_partial_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_uri(dir.path());
        write_to_file(&target, b"first", "raw", Some("9"), 1).unwrap();
        write_to_file(&target, b"second", "raw", Some("9"), 1).unwrap(); // a requeued job
        assert_eq!(
            std::fs::read(dir.path().join("job-9.prn")).unwrap(),
            b"second"
        );
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["job-9.prn"], "no .partial file left over");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_planted_at_the_job_path_is_replaced_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        std::os::unix::fs::symlink(&victim, out.join("job-5.prn")).unwrap();
        write_to_file(&file_uri(&out), b"label", "raw", Some("5"), 1).unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert_eq!(std::fs::read(out.join("job-5.prn")).unwrap(), b"label");
    }

    #[test]
    fn a_hostile_job_id_cannot_leave_the_target_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path =
            write_to_file(&file_uri(dir.path()), b"x", "pdf", Some("../../evil"), 1).unwrap();
        assert!(Path::new(&path).starts_with(dir.path()));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn test_print_job_file_target_routes_to_file_fn_once_per_copy() {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let c = calls.clone();
        let fns = Fns {
            file: Box::new(move |j| {
                c.borrow_mut().push((
                    j.target.to_string(),
                    j.data.to_vec(),
                    j.mode.to_string(),
                    j.job_id.map(String::from),
                    j.index,
                ));
                Ok(())
            }),
            ..Fns::refusing()
        };
        print_job(
            "pdf",
            &entry("archive", true, "file:///srv/inbox"),
            b"%PDF",
            2,
            None,
            &fns,
            Some("3"),
        )
        .unwrap();
        let calls = calls.borrow();
        assert_eq!(calls.iter().map(|c| c.4).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(
            calls[0],
            (
                "file:///srv/inbox".to_string(),
                b"%PDF".to_vec(),
                "pdf".to_string(),
                Some("3".to_string()),
                1
            )
        );
    }

    #[test]
    fn test_print_job_file_target_rejects_unknown_mode() {
        let e = print_job(
            "docx",
            &entry("archive", true, "file:///srv/inbox"),
            b"x",
            1,
            None,
            &Fns::refusing(),
            None,
        )
        .unwrap_err();
        assert!(e.contains("bad mode"));
    }

    #[test]
    fn test_add_capabilities_skips_socket_targets_and_failures() {
        let printers = vec![
            entry("HP", true, "HP"),
            entry("netz", false, "socket://10.0.0.5:9100"),
            entry("Z", false, "Z"),
        ];
        let caps = Capabilities {
            papers: Some(vec!["A4".into()]),
            ..Default::default()
        };
        let c = caps.clone();
        let out = add_capabilities(printers, &move |t| (t == "HP").then(|| c.clone()));
        assert_eq!(out[0].capabilities, Some(caps));
        assert!(out[1].capabilities.is_none() && out[2].capabilities.is_none());
    }

    #[test]
    fn test_add_capabilities_skips_file_targets() {
        let out = add_capabilities(vec![entry("archive", true, "file:///srv/inbox")], &|_| {
            panic!("collector not expected")
        });
        assert!(out[0].capabilities.is_none());
    }
}

// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The dashboard: a static, secret-free Next.js export (source in `web/`, built with
//! `npm run build:app` into `crates/server/web`), embedded into the binary at compile time so the
//! server is one file with no Node and no bundle directory at runtime. It prompts for the API
//! token and calls the JSON endpoints with it, so all data stays behind auth and the bundle
//! itself carries nothing sensitive.
use include_dir::{Dir, include_dir};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

static BUNDLE_DIR: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/web");

/// Served at `/` when the bundle was not built before compiling.
pub const NO_DASHBOARD_HTML: &str = "<!doctype html><meta charset=\"utf-8\"><title>printpapi</title>\n<body style=\"font:15px system-ui;margin:3rem auto;max-width:34rem;padding:0 1rem\">\n<h1>printpapi</h1><p>The dashboard bundle is missing. Build it once:</p>\n<pre>cd web &amp;&amp; npm install &amp;&amp; npm run build:app</pre>\n<p>The JSON API works either way.</p>";

/// Explicit table: guessing from the OS is how a `.js` ends up `text/plain` (the Windows
/// registry routinely says so), which browsers refuse to execute as a module.
const CONTENT_TYPES: &[(&str, &str)] = &[
    (".html", "text/html; charset=utf-8"),
    (".js", "text/javascript; charset=utf-8"),
    (".mjs", "text/javascript; charset=utf-8"),
    (".css", "text/css; charset=utf-8"),
    (".json", "application/json"),
    (".txt", "text/plain; charset=utf-8"),
    (".svg", "image/svg+xml"),
    (".ico", "image/x-icon"),
    (".png", "image/png"),
    (".webmanifest", "application/manifest+json"),
    (".woff", "font/woff"),
    (".woff2", "font/woff2"),
];

/// Content-hashed names live here: safe to cache forever.
const IMMUTABLE_PREFIX: &str = "_next/static/";

pub struct StaticFile {
    pub data: &'static [u8],
    pub content_type: &'static str,
    /// Under `_next/static/`: cache forever. Decided from the resolved file, not the raw URL —
    /// `/_next/static/..%2f..%2findex.html` is inside the bundle but not a hashed asset.
    pub immutable: bool,
}

/// The bundle as a lookup table keyed by `/`-joined relative path, whatever the build OS's
/// separator was.
pub struct Bundle {
    files: HashMap<String, &'static [u8]>,
    dirs: HashSet<String>,
}

impl Bundle {
    pub fn from_dir(root: &'static Dir<'static>) -> Bundle {
        let mut b = Bundle {
            files: HashMap::new(),
            dirs: HashSet::new(),
        };
        b.add(root);
        b
    }

    pub fn from_files(files: &[(&str, &'static [u8])]) -> Bundle {
        let mut b = Bundle {
            files: HashMap::new(),
            dirs: HashSet::new(),
        };
        for (path, data) in files {
            let segs: Vec<&str> = path.split('/').collect();
            for i in 1..segs.len() {
                b.dirs.insert(segs[..i].join("/"));
            }
            b.files.insert(path.to_string(), data);
        }
        b
    }

    fn key(path: &std::path::Path) -> String {
        path.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn add(&mut self, dir: &'static Dir<'static>) {
        for f in dir.files() {
            self.files.insert(Self::key(f.path()), f.contents());
        }
        for d in dir.dirs() {
            self.dirs.insert(Self::key(d.path()));
            self.add(d);
        }
    }

    pub fn has_index(&self) -> bool {
        self.files.contains_key("index.html")
    }

    pub fn lookup(&self, target: &str) -> Option<StaticFile> {
        let raw = target.split('?').next().unwrap_or("");
        let raw = raw.split('#').next().unwrap_or("");
        let rel = percent_decode(raw);
        let mut stack: Vec<&str> = Vec::new();
        // Both separators: the Python server resolved paths with the OS's rules, and on Windows a
        // backslash is a separator too. A `..` that climbs out of the bundle is a traversal.
        for seg in rel.split(['/', '\\']) {
            match seg {
                "" | "." => {}
                ".." => {
                    stack.pop()?;
                }
                s if s.contains('\0') => return None,
                s => stack.push(s),
            }
        }
        let mut key = stack.join("/");
        if key.is_empty() || self.dirs.contains(&key) {
            key = if key.is_empty() {
                "index.html".into()
            } else {
                format!("{key}/index.html")
            };
        }
        let data = *self.files.get(&key)?;
        Some(StaticFile {
            data,
            content_type: content_type(&key),
            immutable: key.starts_with(IMMUTABLE_PREFIX),
        })
    }
}

/// `Path.suffix` of the last segment, looked up in the table (unknown -> octet-stream).
fn content_type(key: &str) -> &'static str {
    let name = key.rsplit('/').next().unwrap_or(key);
    let suffix = match name.rfind('.') {
        Some(i) if i > 0 => name[i..].to_ascii_lowercase(),
        _ => String::new(),
    };
    CONTENT_TYPES
        .iter()
        .find(|(ext, _)| *ext == suffix)
        .map_or("application/octet-stream", |(_, ct)| ct)
}

/// `urllib.parse.unquote`: %XX to bytes, then UTF-8 with replacement; malformed escapes stay.
pub(crate) fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if let (b'%', Some(hi), Some(lo)) = (
            b[i],
            b.get(i + 1).and_then(|&c| hex(c)),
            b.get(i + 2).and_then(|&c| hex(c)),
        ) {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn bundle() -> &'static Bundle {
    static B: OnceLock<Bundle> = OnceLock::new();
    B.get_or_init(|| Bundle::from_dir(&BUNDLE_DIR))
}

/// Map a request target to an embedded file: query/fragment stripped, percent-decoded, `..`
/// resolved inside the bundle (escaping it = None), a directory -> its `index.html`.
pub fn lookup(target: &str) -> Option<StaticFile> {
    bundle().lookup(target)
}

/// Whether the bundle has an `index.html` (false = serve NO_DASHBOARD_HTML at `/`).
pub fn has_index() -> bool {
    bundle().has_index()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bundle {
        Bundle::from_files(&[
            ("index.html", b"<!doctype html>root"),
            ("devices/index.html", b"devices"),
            ("_next/static/chunks/app.js", b"js"),
            ("_next/static/css/a.CSS", b"css"),
            ("devices/__next.devices", b"rsc"),
        ])
    }

    #[test]
    fn root_and_deep_links_resolve_to_their_index() {
        let b = sample();
        assert_eq!(b.lookup("/").unwrap().data, b"<!doctype html>root");
        assert_eq!(
            b.lookup("/?reset=tok").unwrap().data,
            b"<!doctype html>root"
        );
        assert_eq!(b.lookup("/devices/").unwrap().data, b"devices");
        assert_eq!(b.lookup("/devices").unwrap().data, b"devices");
        assert_eq!(b.lookup("/devices/#x").unwrap().data, b"devices");
    }

    #[test]
    fn content_types_and_cache_policy() {
        let b = sample();
        let js = b.lookup("/_next/static/chunks/app.js").unwrap();
        assert_eq!(js.content_type, "text/javascript; charset=utf-8");
        assert!(js.immutable);
        assert_eq!(
            b.lookup("/_next/static/css/a.CSS").unwrap().content_type,
            "text/css; charset=utf-8"
        );
        let html = b.lookup("/").unwrap();
        assert_eq!(html.content_type, "text/html; charset=utf-8");
        assert!(!html.immutable);
        assert_eq!(
            b.lookup("/devices/__next.devices").unwrap().content_type,
            "application/octet-stream"
        );
    }

    #[test]
    fn immutable_is_decided_by_the_resolved_file() {
        let f = sample()
            .lookup("/_next/static/..%2f..%2findex.html")
            .unwrap();
        assert_eq!(f.data, b"<!doctype html>root");
        assert!(!f.immutable);
    }

    #[test]
    fn traversal_nul_and_missing_paths_are_none() {
        let b = sample();
        for p in [
            "/../server.py",
            "/..%2fserver.py",
            "/%2e%2e/store.py",
            "/../../app/store.py",
            "/a%00b",
            "/jobs%00",
            "/_next/static/x%00.js",
            "/nope",
            "/..\\x",
            "/devices/..%5c..%5cx",
        ] {
            assert!(b.lookup(p).is_none(), "{p}");
        }
    }

    #[test]
    fn percent_decoding_matches_unquote() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%e2%82%ac"), "€");
        assert_eq!(percent_decode("bad%zz%2"), "bad%zz%2");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
    }

    #[test]
    fn an_empty_bundle_has_no_index() {
        assert!(!Bundle::from_files(&[]).has_index());
        assert!(sample().has_index());
    }

    #[test]
    fn the_embedded_bundle_has_the_dashboard() {
        assert!(has_index());
        let html = std::str::from_utf8(lookup("/").unwrap().data)
            .unwrap()
            .to_lowercase();
        assert!(html.contains("<!doctype html>") && html.contains("/_next/static/"));
        for route in ["/print/", "/devices/", "/history/", "/keys/", "/downloads/"] {
            assert!(lookup(route).is_some(), "{route}");
        }
    }
}

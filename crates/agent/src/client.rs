// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
//! The agent's HTTP side: bearer auth, extra headers from agent.ini, a socket timeout per phase
//! and per read (like Python's `urlopen(timeout=)`), proxies chosen per URL scheme like `urllib`.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use ureq::config::Config;
use ureq::tls::{RootCerts, TlsConfig};

use crate::config::HttpSettings;

const UA: &str = "printpapi-agent";
/// Sent on every request to the server: the claim then carries `job_uid` (docs/api.md). A server
/// or proxy that does not know the header ignores it; the agent then works as before.
const FEATURES: (&str, &str) = ("X-Printapi-Features", "job-uid");
/// ponytail: flat caps so a hostile or broken server cannot make the agent allocate without bound.
/// The server itself caps request bodies at 32 MB.
const MAX_JSON_BODY: u64 = 32 * 1024 * 1024;
const MAX_PAYLOAD: u64 = 128 * 1024 * 1024;

/// Why a call failed. The split matters to the retry loops: a status code or a dead connection
/// (Python's `OSError`) is retried everywhere, a body that is not JSON (Python's `ValueError`)
/// only where the caller says so.
#[derive(Debug, Clone, PartialEq)]
pub enum HttpError {
    /// `server returned 503`, `connection failed: ...`
    Os(String),
    /// A reply that is not the JSON we expected, or a request we could not even build.
    Value(String),
}

/// The status code of a `server returned NNN` error, `None` for any other failure.
pub fn http_status(e: &HttpError) -> Option<u16> {
    let HttpError::Os(m) = e else { return None };
    m.strip_prefix("server returned ")?.parse().ok()
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Os(m) | HttpError::Value(m) => f.write_str(m),
        }
    }
}

/// What the run loop needs from the network. Tests substitute a fake; [`Client`] is the real one.
pub trait Http {
    /// `GET url` -> parsed JSON; `None` on 204 (no job).
    fn get(&self, url: &str, key: &str) -> Result<Option<Value>, HttpError>;
    /// `GET url` -> raw body; `None` on 204.
    fn get_bytes(&self, url: &str, key: &str) -> Result<Option<Vec<u8>>, HttpError>;
    /// `POST url` with a JSON body -> parsed JSON; `None` on 204.
    fn post(&self, url: &str, key: &str, body: &Value) -> Result<Option<Value>, HttpError>;
}

/// Which agent serves a URL.
#[derive(Debug, Clone, PartialEq)]
enum ProxyPlan {
    /// No `http_proxy`/`https_proxy` in the environment: ureq's own discovery, which on Windows
    /// reads the Internet Settings proxy (what `urllib.request.getproxies()` does there).
    Default,
    Direct,
    Via,
}

/// The proxy variables, read once.
#[derive(Debug, Clone, Default)]
struct ProxyEnv {
    http: Option<String>,
    https: Option<String>,
    no_proxy: Option<String>,
}

impl ProxyEnv {
    fn from_env() -> ProxyEnv {
        // urllib: the lower-case name wins, an empty value counts as unset.
        let get = |name: &str| {
            [name.to_lowercase(), name.to_uppercase()]
                .iter()
                .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
        };
        ProxyEnv {
            http: get("http_proxy"),
            https: get("https_proxy"),
            no_proxy: get("no_proxy"),
        }
    }

    /// `urllib`: the proxy is looked up by the URL's scheme only; `no_proxy` entries bypass it.
    fn plan(&self, url: &str) -> ProxyPlan {
        if self.http.is_none() && self.https.is_none() {
            return ProxyPlan::Default;
        }
        let (https, rest) = match url.split_once("://") {
            Some((s, rest)) => (s.eq_ignore_ascii_case("https"), rest),
            None => (false, url),
        };
        let proxy = if https { &self.https } else { &self.http };
        if proxy.is_none() || self.bypassed(host_of(rest)) {
            ProxyPlan::Direct
        } else {
            ProxyPlan::Via
        }
    }

    fn bypassed(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.no_proxy.iter().flat_map(|n| n.split(',')).any(|e| {
            let e = e.trim().to_lowercase();
            let e = e.trim_start_matches('*').trim_start_matches('.');
            // "*" and ".*" strip to "" and match everything
            e.is_empty() || e == host || host.ends_with(&format!(".{e}"))
        })
    }
}

/// The host of `authority/path` (no scheme): userinfo, port and IPv6 brackets removed.
fn host_of(rest: &str) -> &str {
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let auth = auth.rsplit('@').next().unwrap_or(auth);
    if let Some(v6) = auth.strip_prefix('[') {
        return v6.split(']').next().unwrap_or(v6);
    }
    auth.split(':').next().unwrap_or(auth)
}

pub struct Client {
    default: ureq::Agent,
    direct: ureq::Agent,
    http: Option<ureq::Agent>,
    https: Option<ureq::Agent>,
    env: ProxyEnv,
    headers: Vec<(String, String)>,
}

impl Client {
    pub fn new(settings: &HttpSettings) -> Client {
        Client::with_env(settings, ProxyEnv::from_env())
    }

    fn with_env(settings: &HttpSettings, env: ProxyEnv) -> Client {
        let timeout = Duration::try_from_secs_f64(settings.timeout).ok();
        // ponytail: no global cap on purpose. Python's timeout bounded each socket operation, so
        // a big payload on a slow link still downloads; only a stalled connection times out.
        let build = |proxy: Option<Option<ureq::Proxy>>| {
            let mut b = Config::builder()
                .timeout_resolve(timeout)
                .timeout_connect(timeout)
                .timeout_send_request(timeout)
                .timeout_recv_response(timeout)
                .timeout_per_read(timeout)
                .timeout_per_write(timeout)
                .http_status_as_error(false)
                .user_agent(UA)
                .tls_config(
                    TlsConfig::builder()
                        .root_certs(RootCerts::PlatformVerifier)
                        .build(),
                );
            if let Some(p) = proxy {
                b = b.proxy(p);
            }
            ureq::Agent::new_with_config(b.build())
        };
        let via = |p: &Option<String>| {
            let p = p.as_deref()?;
            let p = if p.contains("://") {
                p.to_string()
            } else {
                format!("http://{p}")
            };
            // an unparsable proxy value is skipped: that scheme then goes direct
            ureq::Proxy::new(&p).ok().map(|p| build(Some(Some(p))))
        };
        Client {
            default: build(None),
            direct: build(Some(None)),
            http: via(&env.http),
            https: via(&env.https),
            env,
            headers: settings
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        }
    }

    fn agent_for(&self, url: &str) -> &ureq::Agent {
        let https = url
            .split_once("://")
            .is_some_and(|(s, _)| s.eq_ignore_ascii_case("https"));
        match self.env.plan(url) {
            ProxyPlan::Default => &self.default,
            ProxyPlan::Via => (if https { &self.https } else { &self.http })
                .as_ref()
                .unwrap_or(&self.direct),
            ProxyPlan::Direct => &self.direct,
        }
    }

    fn request(
        &self,
        url: &str,
        key: &str,
        body: Option<&Value>,
        limit: u64,
    ) -> Result<Option<Vec<u8>>, HttpError> {
        let result = match body {
            None => {
                let mut r = self.agent_for(url).get(url);
                for (k, v) in &self.headers {
                    r = r.header(k, v);
                }
                r.header("Authorization", format!("Bearer {key}"))
                    .header("User-Agent", UA)
                    .header(FEATURES.0, FEATURES.1)
                    .call()
            }
            Some(b) => {
                let mut r = self.agent_for(url).post(url);
                for (k, v) in &self.headers {
                    r = r.header(k, v);
                }
                r.header("Authorization", format!("Bearer {key}"))
                    .header("User-Agent", UA)
                    .header(FEATURES.0, FEATURES.1)
                    .header("Content-Type", "application/json")
                    .send(b.to_string().into_bytes())
            }
        };
        let resp = result.map_err(map_err)?;
        let status = resp.status().as_u16();
        if status == 204 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(HttpError::Os(format!("server returned {status}")));
        }
        let mut body = resp.into_body();
        body.with_config()
            .limit(limit)
            .read_to_vec()
            .map(Some)
            .map_err(map_err)
    }

    fn json(&self, url: &str, key: &str, body: Option<&Value>) -> Result<Option<Value>, HttpError> {
        match self.request(url, key, body, MAX_JSON_BODY)? {
            None => Ok(None),
            Some(raw) if raw.is_empty() => Ok(Some(Value::Object(Default::default()))),
            Some(raw) => serde_json::from_slice(&raw)
                .map(Some)
                .map_err(|e| HttpError::Value(e.to_string())),
        }
    }
}

/// [`download`] as a value, for a caller that can be given a fake.
pub type DownloadFn<'a> = dyn Fn(&str, u64, Duration) -> Result<Vec<u8>, DownloadError> + 'a;

/// A browser's User-Agent. A WAF or Cloudflare in front of a download host answers a bare HTTP
/// client with 403 (AGENTS.md, pitfall 3), so downloads send this and never the agent's own.
pub const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

/// Why [`download`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadError {
    /// The server says there is no such file (404).
    NotFound,
    /// Anything else: the text names what went wrong and never holds a header value.
    Other(String),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DownloadError::NotFound => f.write_str("server returned 404"),
            DownloadError::Other(m) => f.write_str(m),
        }
    }
}

/// `GET url` into memory, for a file that is not for the server of this agent: an update, a
/// SumatraPDF. http and https only; the proxy choice of every other call; the browser
/// User-Agent; **no Authorization and none of the `[headers]` of agent.ini**, because the host is
/// not the printpapi server. `limit` bytes at most, and `deadline` for the whole call, redirects
/// and body included (ureq's `timeout_global`: "the whole request including redirects", checked in
/// ureq 3.5.0 src/config.rs `timeout_global`), so a host that trickles bytes cannot hold it.
pub fn download(url: &str, limit: u64, deadline: Duration) -> Result<Vec<u8>, DownloadError> {
    download_with(&ProxyEnv::from_env(), url, limit, deadline)
}

fn download_with(
    env: &ProxyEnv,
    url: &str,
    limit: u64,
    deadline: Duration,
) -> Result<Vec<u8>, DownloadError> {
    let scheme_ok = url
        .split_once("://")
        .is_some_and(|(s, _)| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https"));
    if !scheme_ok {
        return Err(DownloadError::Other(
            "only http and https can be downloaded".into(),
        ));
    }
    let mut builder = Config::builder()
        .timeout_global(Some(deadline))
        .http_status_as_error(false)
        .user_agent(BROWSER_UA)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        );
    match env.plan(url) {
        ProxyPlan::Default => {}
        ProxyPlan::Direct => builder = builder.proxy(None),
        ProxyPlan::Via => {
            let https = url
                .split_once("://")
                .is_some_and(|(s, _)| s.eq_ignore_ascii_case("https"));
            let value = if https { &env.https } else { &env.http };
            let proxy = value.as_deref().and_then(|p| {
                let p = if p.contains("://") {
                    p.to_string()
                } else {
                    format!("http://{p}")
                };
                ureq::Proxy::new(&p).ok()
            });
            // an unparsable proxy value is skipped: that scheme then goes direct, as in Client
            builder = builder.proxy(proxy);
        }
    }
    let agent = ureq::Agent::new_with_config(builder.build());
    let mut request = agent.get(url);
    // a bare GET: the User-Agent is the browser's, nothing else is added
    request = request.header("Accept", "*/*");
    let resp = request
        .call()
        .map_err(|e| DownloadError::Other(map_err(e).to_string()))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Err(DownloadError::NotFound);
    }
    if !(200..300).contains(&status) {
        return Err(DownloadError::Other(format!("server returned {status}")));
    }
    // ureq's LimitReader fails the read that looks for the end of a body of exactly `limit`
    // bytes (src/body/limit.rs, 3.5.0), so ask for one byte more and compare afterwards
    let body = resp
        .into_body()
        .with_config()
        .limit(limit.saturating_add(1))
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => too_large(limit),
            other => DownloadError::Other(map_err(other).to_string()),
        })?;
    if body.len() as u64 > limit {
        return Err(too_large(limit));
    }
    Ok(body)
}

fn too_large(limit: u64) -> DownloadError {
    DownloadError::Other(format!("the file is larger than {limit} bytes"))
}

fn map_err(e: ureq::Error) -> HttpError {
    match e {
        ureq::Error::Http(_) | ureq::Error::BadUri(_) => HttpError::Value(e.to_string()),
        ureq::Error::Timeout(_) => HttpError::Os("connection failed: timed out".into()),
        ureq::Error::BodyExceedsLimit(n) => {
            HttpError::Os(format!("payload larger than {} MB", n / (1024 * 1024)))
        }
        other => HttpError::Os(format!("connection failed: {other}")),
    }
}

impl Http for Client {
    fn get(&self, url: &str, key: &str) -> Result<Option<Value>, HttpError> {
        self.json(url, key, None)
    }

    fn get_bytes(&self, url: &str, key: &str) -> Result<Option<Vec<u8>>, HttpError> {
        self.request(url, key, None, MAX_PAYLOAD)
    }

    fn post(&self, url: &str, key: &str, body: &Value) -> Result<Option<Value>, HttpError> {
        self.json(url, key, Some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A one-shot loopback HTTP server: answers the first request with `reply` and sends the raw
    /// request text back through the channel. `None` reply = accept and then stay silent.
    fn serve(reply: Option<&'static str>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = s.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).into_owned();
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len {
                        break;
                    }
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
            match reply {
                Some(r) => {
                    let _ = s.write_all(r.as_bytes());
                }
                None => std::thread::sleep(Duration::from_secs(3)),
            }
        });
        (url, rx)
    }

    fn client(timeout: f64, headers: &[(&str, &str)]) -> Client {
        let headers: BTreeMap<String, String> = headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Client::new(&HttpSettings { headers, timeout })
    }

    const OK_JSON: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"ok\": true}";

    #[test]
    fn http_status_reads_only_status_errors() {
        assert_eq!(
            http_status(&HttpError::Os("server returned 409".into())),
            Some(409)
        );
        assert_eq!(
            http_status(&HttpError::Os("server returned 503".into())),
            Some(503)
        );
        assert_eq!(
            http_status(&HttpError::Os(
                "connection failed: server returned 409".into()
            )),
            None
        );
        assert_eq!(
            http_status(&HttpError::Os("connection failed: timed out".into())),
            None
        );
        assert_eq!(
            http_status(&HttpError::Os("server returned abc".into())),
            None
        );
        assert_eq!(
            http_status(&HttpError::Value("server returned 409".into())),
            None
        );
    }

    #[test]
    fn test_req_sends_extra_headers_and_timeout() {
        let (url, rx) = serve(Some(OK_JSON));
        let c = client(
            45.0,
            &[
                ("cf-access-client-id", "id1"),
                ("cf-access-client-secret", "s3"),
            ],
        );
        assert_eq!(
            c.get(&format!("{url}/agent/jobs"), "k").unwrap(),
            Some(json!({"ok": true}))
        );
        let req = rx.recv().unwrap().to_lowercase();
        assert!(req.starts_with("get /agent/jobs http/1.1"));
        assert!(req.contains("cf-access-client-id: id1"));
        assert!(req.contains("cf-access-client-secret: s3"));
        assert!(req.contains("authorization: bearer k"));
        assert!(req.contains("user-agent: printpapi-agent"));
    }

    #[test]
    fn every_request_asks_for_the_job_identity() {
        // GET, GET of a payload and POST: the server hands out job_uid only to who asks
        let (url, rx) = serve(Some(OK_JSON));
        let c = client(45.0, &[]);
        c.get(&format!("{url}/agent/jobs"), "k").unwrap();
        let req = rx.recv().unwrap().to_lowercase();
        assert!(req.contains("x-printapi-features: job-uid\r\n"), "{req}");
        let (url, rx) = serve(Some(OK_JSON));
        c.get_bytes(&format!("{url}/agent/jobs/1/payload"), "k")
            .unwrap();
        assert!(
            rx.recv()
                .unwrap()
                .to_lowercase()
                .contains("x-printapi-features: job-uid\r\n")
        );
        let (url, rx) = serve(Some(OK_JSON));
        c.post(&format!("{url}/agent/results"), "k", &json!({}))
            .unwrap();
        assert!(
            rx.recv()
                .unwrap()
                .to_lowercase()
                .contains("x-printapi-features: job-uid\r\n")
        );
    }

    #[test]
    fn test_req_has_a_default_timeout() {
        // a server that accepts and never answers must end in a timeout error, not hang
        let (url, _rx) = serve(None);
        let c = client(0.4, &[]);
        let started = std::time::Instant::now();
        let e = c.get(&format!("{url}/x"), "k").unwrap_err();
        assert!(
            matches!(&e, HttpError::Os(m) if m.starts_with("connection failed")),
            "{e}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let c = client(60.0, &[]);
        assert_eq!(c.default.config().timeouts().global, None);
        assert_eq!(
            c.default.config().timeout_per_read(),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            c.default.config().timeouts().recv_response,
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn a_slow_but_steady_payload_download_is_not_cut_off() {
        // 8 bytes at 150 ms each take about 1.2 s in total, more than the 0.8 s timeout, but no
        // single read stalls that long (Python's per-socket-operation timeout).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n");
            for b in b"%PDF-1.4" {
                std::thread::sleep(Duration::from_millis(150));
                let _ = s.write_all(&[*b]);
                let _ = s.flush();
            }
        });
        let got = client(0.8, &[])
            .get_bytes(&format!("{url}/p"), "k")
            .unwrap();
        assert_eq!(got, Some(b"%PDF-1.4".to_vec()));
    }

    /// A loopback server that answers one connection per entry of `replies`, in order, and sends
    /// each raw request back.
    fn serve_many(replies: Vec<String>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (url, rx)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const DEADLINE: Duration = Duration::from_secs(10);

    #[test]
    fn download_sends_the_browser_ua_and_neither_the_key_nor_the_headers() {
        let (url, rx) = serve_many(vec![ok("file bytes")]);
        let got = download_with(
            &ProxyEnv::default(),
            &format!("{url}/a.bin"),
            1000,
            DEADLINE,
        )
        .unwrap();
        assert_eq!(got, b"file bytes");
        let req = rx.recv().unwrap().to_lowercase();
        assert!(req.starts_with("get /a.bin http/1.1"), "{req}");
        assert!(
            req.contains(&format!("user-agent: {}", BROWSER_UA.to_lowercase())),
            "{req}"
        );
        assert!(!req.contains("authorization"), "{req}");
        assert!(!req.contains("printpapi-agent"), "{req}");
        assert!(!req.contains("cf-access"), "{req}");
    }

    #[test]
    fn download_follows_a_redirect_without_adding_anything() {
        let (url, rx) = serve_many(vec![
            "HTTP/1.1 302 Found\r\nLocation: /real\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            ok("moved"),
        ]);
        let got = download_with(&ProxyEnv::default(), &format!("{url}/a"), 1000, DEADLINE).unwrap();
        assert_eq!(got, b"moved");
        assert!(rx.recv().unwrap().starts_with("GET /a "));
        let second = rx.recv().unwrap().to_lowercase();
        assert!(second.starts_with("get /real "), "{second}");
        assert!(!second.contains("authorization"));
    }

    #[test]
    fn download_enforces_the_size_limit() {
        let (url, _rx) = serve_many(vec![ok("0123456789")]);
        let e = download_with(&ProxyEnv::default(), &format!("{url}/a"), 5, DEADLINE).unwrap_err();
        assert!(
            matches!(&e, DownloadError::Other(m) if m.contains("larger than 5 bytes")),
            "{e}"
        );
    }

    #[test]
    fn download_takes_a_body_of_exactly_the_limit_and_refuses_one_byte_more() {
        // ureq's LimitReader fails the read that looks for the end of a body that is exactly
        // `limit` long, and both callers ask for the size the signed file names
        let (url, _rx) = serve_many(vec![ok("0123456789")]);
        let got = download_with(&ProxyEnv::default(), &format!("{url}/a"), 10, DEADLINE).unwrap();
        assert_eq!(got, b"0123456789");
        let (url, _rx) = serve_many(vec![ok("0123456789")]);
        let e = download_with(&ProxyEnv::default(), &format!("{url}/a"), 9, DEADLINE).unwrap_err();
        assert!(
            matches!(&e, DownloadError::Other(m) if m.contains("larger than 9 bytes")),
            "{e}"
        );
    }

    #[test]
    fn download_enforces_one_deadline_for_the_whole_call() {
        // headers arrive at once, the body stalls: only a global deadline ends this
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc");
            std::thread::sleep(Duration::from_secs(4));
        });
        let started = std::time::Instant::now();
        let e = download_with(
            &ProxyEnv::default(),
            &format!("{url}/a"),
            1000,
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(matches!(e, DownloadError::Other(_)), "{e}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn download_says_not_found_for_404_and_names_other_statuses() {
        let (url, _rx) = serve_many(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
            "HTTP/1.1 503 Busy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        ]);
        assert_eq!(
            download_with(&ProxyEnv::default(), &format!("{url}/a"), 10, DEADLINE),
            Err(DownloadError::NotFound)
        );
        assert_eq!(
            download_with(&ProxyEnv::default(), &format!("{url}/a"), 10, DEADLINE),
            Err(DownloadError::Other("server returned 503".into()))
        );
    }

    #[test]
    fn download_refuses_everything_but_http_and_https() {
        for url in ["file:///c:/x", "ftp://h/x", "h/x", "javascript:1", ""] {
            let e = download_with(&ProxyEnv::default(), url, 10, DEADLINE).unwrap_err();
            assert!(
                matches!(&e, DownloadError::Other(m) if m.contains("only http and https")),
                "{url}"
            );
        }
    }

    #[test]
    fn download_uses_the_proxy_of_the_url_scheme_and_skips_no_proxy_hosts() {
        // ureq talks to an HTTP proxy with CONNECT: the loopback server plays a proxy that refuses
        // it, which is enough to see where the request went
        let refuse = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (proxy, rx) = serve_many(vec![refuse.to_string()]);
        let env = penv(Some(&proxy), None, None);
        assert!(download_with(&env, "http://files.example.invalid/x.bin", 100, DEADLINE).is_err());
        let req = rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .to_lowercase();
        assert!(
            req.starts_with("connect files.example.invalid:80 http/1.1"),
            "{req}"
        );
        // only HTTPS_PROXY is set: a plain http URL goes direct, and the proxy hears nothing
        let (proxy, rx) = serve_many(vec![refuse.to_string()]);
        let env = penv(None, Some(&proxy), None);
        assert!(download_with(&env, "http://files.example.invalid/x.bin", 100, DEADLINE).is_err());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // no_proxy: that host goes direct as well
        let (proxy, rx) = serve_many(vec![refuse.to_string()]);
        let env = penv(Some(&proxy), None, Some("files.example.invalid"));
        assert!(download_with(&env, "http://files.example.invalid/x.bin", 100, DEADLINE).is_err());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn an_oversized_payload_says_so() {
        assert_eq!(
            map_err(ureq::Error::BodyExceedsLimit(MAX_PAYLOAD)),
            HttpError::Os("payload larger than 128 MB".into())
        );
    }

    fn penv(http: Option<&str>, https: Option<&str>, no: Option<&str>) -> ProxyEnv {
        ProxyEnv {
            http: http.map(Into::into),
            https: https.map(Into::into),
            no_proxy: no.map(Into::into),
        }
    }

    #[test]
    fn proxies_are_picked_per_url_scheme_like_urllib() {
        // only HTTPS_PROXY: a plain http URL goes direct
        let e = penv(None, Some("http://127.0.0.1:9"), None);
        assert_eq!(e.plan("http://h/agent/jobs"), ProxyPlan::Direct);
        assert_eq!(e.plan("https://h/agent/jobs"), ProxyPlan::Via);
        let e = penv(Some("proxy:3128"), None, None);
        assert_eq!(e.plan("http://h/x"), ProxyPlan::Via);
        assert_eq!(e.plan("https://h/x"), ProxyPlan::Direct);
        // nothing set: ureq's default discovery (the Windows system proxy)
        assert_eq!(
            penv(None, None, None).plan("http://h/x"),
            ProxyPlan::Default
        );
    }

    #[test]
    fn no_proxy_bypasses_the_proxy() {
        let e = penv(
            Some("p:1"),
            Some("p:1"),
            Some("localhost, .corp.example,10.0.0.5"),
        );
        assert_eq!(e.plan("http://localhost:8080/x"), ProxyPlan::Direct);
        assert_eq!(e.plan("https://print.corp.example/x"), ProxyPlan::Direct);
        assert_eq!(e.plan("http://10.0.0.5/x"), ProxyPlan::Direct);
        assert_eq!(e.plan("http://u:p@other.example:80/x"), ProxyPlan::Via);
        assert_eq!(e.plan("http://[::1]:80/x"), ProxyPlan::Via);
        let all = penv(Some("p:1"), None, Some("*"));
        assert_eq!(all.plan("http://anything/x"), ProxyPlan::Direct);
    }

    #[test]
    fn a_closed_https_proxy_does_not_affect_an_http_url() {
        let (url, _rx) = serve(Some(OK_JSON));
        let c = Client::with_env(
            &HttpSettings {
                headers: BTreeMap::new(),
                timeout: 5.0,
            },
            penv(None, Some("http://127.0.0.1:9"), None),
        );
        assert_eq!(
            c.get(&format!("{url}/x"), "k").unwrap(),
            Some(json!({"ok": true}))
        );
    }

    #[test]
    fn test_req_maps_urlerror_to_oserror() {
        // port 1 on loopback refuses the connection
        let e = client(30.0, &[])
            .get("http://127.0.0.1:1/x", "k")
            .unwrap_err();
        assert!(
            matches!(&e, HttpError::Os(m) if m.starts_with("connection failed")),
            "{e}"
        );
    }

    #[test]
    fn non_2xx_is_server_returned_code() {
        let (url, _rx) = serve(Some(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ));
        let e = client(30.0, &[]).get(&format!("{url}/x"), "k").unwrap_err();
        assert_eq!(e, HttpError::Os("server returned 503".into()));
    }

    #[test]
    fn status_204_is_none_for_json_and_bytes() {
        const NO_CONTENT: &str = "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        let (url, _rx) = serve(Some(NO_CONTENT));
        assert_eq!(
            client(30.0, &[]).get(&format!("{url}/x"), "k").unwrap(),
            None
        );
        let (url, _rx) = serve(Some(NO_CONTENT));
        assert_eq!(
            client(30.0, &[])
                .get_bytes(&format!("{url}/x"), "k")
                .unwrap(),
            None
        );
    }

    #[test]
    fn non_json_body_is_its_own_error_kind() {
        let (url, _rx) = serve(Some(
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n<html>",
        ));
        let e = client(30.0, &[]).get(&format!("{url}/x"), "k").unwrap_err();
        assert!(matches!(e, HttpError::Value(_)), "{e:?}");
    }

    #[test]
    fn empty_200_body_is_an_empty_object_and_bytes_pass_through() {
        let (url, _rx) = serve(Some(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ));
        assert_eq!(
            client(30.0, &[]).get(&format!("{url}/x"), "k").unwrap(),
            Some(json!({}))
        );
        let (url, _rx) = serve(Some(
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n%PDF",
        ));
        assert_eq!(
            client(30.0, &[])
                .get_bytes(&format!("{url}/x"), "k")
                .unwrap(),
            Some(b"%PDF".to_vec())
        );
    }

    #[test]
    fn post_sends_json_content_type_and_body() {
        let (url, rx) = serve(Some(OK_JSON));
        let r = client(30.0, &[])
            .post(
                &format!("{url}/agent/register"),
                "k",
                &json!({"name": "pc"}),
            )
            .unwrap();
        assert_eq!(r, Some(json!({"ok": true})));
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /agent/register HTTP/1.1"));
        assert!(
            req.to_lowercase()
                .contains("content-type: application/json")
        );
        assert!(req.ends_with("{\"name\":\"pc\"}"));
    }

    #[test]
    fn a_bad_header_is_a_value_error_not_a_panic() {
        let e = client(30.0, &[("bad name", "v")])
            .get("http://127.0.0.1:1/x", "k")
            .unwrap_err();
        assert!(matches!(e, HttpError::Value(_)), "{e:?}");
    }
}

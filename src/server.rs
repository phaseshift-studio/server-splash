//! A tiny static file server, so the container can be the dashboard's host.
//!
//! The alternative — generating into a bind-mounted web root — still works and is the
//! right choice if you already run Apache or nginx. Serving it here just means
//! `docker compose up` is enough: no web server to configure, no file ownership to
//! get right, no document root to find.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// How long a client has to send its request before we give up on it.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a request goes: one of this server's own endpoints, or a file under the root.
#[derive(Debug, PartialEq)]
pub(crate) enum Route<'a> {
    Health,
    /// The live peer list behind the page's Resync button.
    LivePeers,
    File(&'a str),
}

/// Classify a request path. Split out from the request loop so it can be tested without a
/// socket.
pub(crate) fn route(path: &str) -> Route<'_> {
    if path == "/healthz" {
        return Route::Health;
    }
    if path == crate::live::LIVE_PATH {
        return Route::LivePeers;
    }
    Route::File(path)
}

/// Serve `root` on `port` until the process is killed.
///
/// `live` is present when this binary is the one serving the page, and answers the peer
/// endpoint the Resync button calls. When the page is deployed to another web server that
/// endpoint does not exist there, and the button falls back to what the page was built
/// with.
pub(crate) fn serve(root: &Path, port: u16, live: Option<crate::live::Context>) -> Result<(), String> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .map_err(|e| format!("cannot listen on port {port}: {e}"))?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.to_path_buf();
                let live = live.clone();
                // One thread per request: the traffic here is a browser and a few
                // health checks, not a workload.
                std::thread::spawn(move || {
                    if let Err(e) = handle(stream, &root, live.as_ref()) {
                        // A client that hangs up mid-response is routine, not an error
                        // worth printing on every page load.
                        let _ = e;
                    }
                });
            }
            Err(e) => eprintln!("  \x1b[33mwarning\x1b[0m: connection failed: {e}"),
        }
    }
    Ok(())
}

/// Exit 0 if something is serving on `port`. Used as the container health check, so
/// the image does not need to ship curl or wget just to answer "is it up?".
pub(crate) fn check(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], port).into(),
        Duration::from_secs(2),
    )
    .is_ok()
}

fn handle(stream: TcpStream, root: &Path, live: Option<&crate::live::Context>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(READ_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");

    // Drain the headers so the client is not left waiting on us.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
    }

    let mut stream = stream;
    if method != "GET" && method != "HEAD" {
        return respond(&mut stream, 405, "text/plain; charset=utf-8", b"method not allowed", method == "HEAD");
    }

    let path = target.split(['?', '#']).next().unwrap_or("/");
    let head_only = method == "HEAD";

    match route(path) {
        Route::Health => respond(&mut stream, 200, "text/plain; charset=utf-8", b"ok\n", head_only),
        Route::LivePeers => match live {
            Some(live) => {
                // Assembled per request, because the whole point is that it reflects the
                // mDNS browse right now rather than at the last regeneration.
                let body = live.peers_json();
                respond(&mut stream, 200, "application/json", body.as_bytes(), head_only)
            }
            None => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found", head_only),
        },
        Route::File(path) => {
            let Some(file) = resolve(root, path) else {
                return respond(&mut stream, 400, "text/plain; charset=utf-8", b"bad request", head_only);
            };
            match std::fs::read(&file) {
                Ok(body) => respond(&mut stream, 200, content_type(&file), &body, head_only),
                Err(_) => respond(&mut stream, 404, "text/plain; charset=utf-8", b"not found", head_only),
            }
        }
    }
}

/// Map a URL path onto a file inside `root`, refusing anything that escapes it.
///
/// Returns `None` for a path that tries to traverse upwards or contains a component we
/// will not serve, so a crafted request cannot read `/etc/shadow`.
pub(crate) fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path);
    let relative = decoded.trim_start_matches('/');
    let relative = if relative.is_empty() { "index.html" } else { relative };
    // A directory request (`/preview/`) means its index.
    let relative = if relative.ends_with('/') {
        format!("{relative}index.html")
    } else {
        relative.to_string()
    };

    let mut out = root.to_path_buf();
    for component in Path::new(&relative).components() {
        match component {
            Component::Normal(part) => out.push(part),
            // `.` is harmless; `..`, a root prefix or a Windows prefix is not.
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// Minimal percent-decoding, so `%20` in a filename works.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Content type from the file extension. Anything unknown is served as bytes rather
/// than guessed at, which is what makes the speed-test payload download properly.
pub(crate) fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    head_only: bool,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// Read a request body, used by nothing yet but kept honest with `Read`.
#[allow(dead_code)]
fn drain(stream: &mut TcpStream, len: usize) -> std::io::Result<()> {
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_index_for_a_directory_request() {
        let root = Path::new("/srv/www");
        assert_eq!(resolve(root, "/").unwrap(), PathBuf::from("/srv/www/index.html"));
        assert_eq!(resolve(root, "/preview/").unwrap(), PathBuf::from("/srv/www/preview/index.html"));
        assert_eq!(resolve(root, "/index.html").unwrap(), PathBuf::from("/srv/www/index.html"));
        assert_eq!(resolve(root, "/icons/metatron.png").unwrap(), PathBuf::from("/srv/www/icons/metatron.png"));
    }

    #[test]
    fn routes_its_own_endpoints_before_the_document_root() {
        assert_eq!(route("/healthz"), Route::Health);
        assert_eq!(route("/_splash/peers"), Route::LivePeers);
        // Everything else is a file, including paths that only look like endpoints.
        assert_eq!(route("/"), Route::File("/"));
        assert_eq!(route("/_splash/other"), Route::File("/_splash/other"));
        assert_eq!(route("/peers.json"), Route::File("/peers.json"));
        // Query strings are stripped before routing, so the cache-buster is harmless.
        assert_eq!(route("/_splash/peers?cb=1"), Route::File("/_splash/peers?cb=1"));
    }

    #[test]
    fn serves_the_files_peers_read() {
        let root = Path::new("/srv/www");
        // The peer endpoints are plain files in the document root, so they work whether
        // the page is served by this binary or by nginx.
        assert_eq!(
            resolve(root, "/.well-known/splash.json").unwrap(),
            PathBuf::from("/srv/www/.well-known/splash.json")
        );
        assert_eq!(resolve(root, "/peers.json").unwrap(), PathBuf::from("/srv/www/peers.json"));
        assert_eq!(content_type(Path::new("splash.json")), "application/json");
    }

    #[test]
    fn refuses_to_escape_the_document_root() {
        let root = Path::new("/srv/www");
        assert!(resolve(root, "/../../etc/shadow").is_none(), "traversal must be refused");
        assert!(resolve(root, "/a/../../etc/passwd").is_none());
        assert!(resolve(root, "/..").is_none());
        // Encoded traversal is decoded first, so it is caught too.
        assert!(resolve(root, "/%2e%2e/%2e%2e/etc/shadow").is_none());
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/a%20b.png"), "/a b.png");
        assert_eq!(percent_decode("/plain.png"), "/plain.png");
        assert_eq!(percent_decode("/100%25.png"), "/100%.png");
        // A truncated escape must not panic or swallow the rest.
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }

    #[test]
    fn types_the_files_the_page_needs() {
        assert_eq!(content_type(Path::new("index.html")), "text/html; charset=utf-8");
        assert_eq!(content_type(Path::new("a.PNG")), "image/png");
        assert_eq!(content_type(Path::new("speedtest.bin")), "application/octet-stream");
        assert_eq!(content_type(Path::new("dashboard.json")), "application/json");
        assert_eq!(content_type(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn health_check_reports_a_missing_server() {
        // Port 1 is not something a test environment serves on.
        assert!(!check(1));
    }
}

//! Dev server: serves files from `dist/`, plus a single SSE endpoint
//! (`/__brief_web_events`) used by the injected reload script.
//!
//! Implemented directly on top of `std::net::TcpListener` to avoid pulling in
//! a full HTTP framework for what is, in essence, a tiny static-file server
//! used during local development.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread;

/// Broadcast channel over which build completions are announced. Each
/// connected SSE client owns one Receiver; the watcher thread pushes
/// `ReloadEvent::Reload` into the broadcaster.
#[derive(Clone)]
pub struct ReloadBroadcaster {
    inner: Arc<Mutex<Vec<Sender<ReloadEvent>>>>,
}

#[derive(Clone, Debug)]
pub enum ReloadEvent {
    Reload,
    BuildError(String),
}

impl ReloadBroadcaster {
    pub fn new() -> Self {
        ReloadBroadcaster {
            inner: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn subscribe(&self) -> Receiver<ReloadEvent> {
        let (tx, rx) = channel();
        self.inner.lock().unwrap().push(tx);
        rx
    }

    pub fn broadcast(&self, ev: ReloadEvent) {
        let mut guard = self.inner.lock().unwrap();
        guard.retain(|tx| tx.send(ev.clone()).is_ok());
    }
}

pub struct ServerOptions {
    pub host: String,
    pub port: u16,
    pub root: PathBuf,
    pub broadcaster: ReloadBroadcaster,
}

/// Start the dev server. Blocks the caller's thread by accepting connections;
/// each connection runs on its own thread.
pub fn run(opts: ServerOptions) -> Result<(), String> {
    let bind = format!("{}:{}", opts.host, opts.port);
    let listener = TcpListener::bind(&bind).map_err(|e| format!("bind {}: {}", bind, e))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("set_nonblocking: {}", e))?;
    eprintln!(
        "brief-web: serving {} at http://{}",
        opts.root.display(),
        bind
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = opts.root.clone();
                let bcast = opts.broadcaster.clone();
                thread::spawn(move || {
                    if let Err(e) = handle(stream, &root, &bcast) {
                        eprintln!("brief-web: connection error: {}", e);
                    }
                });
            }
            Err(e) => eprintln!("brief-web: accept error: {}", e),
        }
    }
    Ok(())
}

fn handle(
    mut stream: TcpStream,
    root: &Path,
    broadcaster: &ReloadBroadcaster,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    let req = match read_request(&mut stream)? {
        Some(r) => r,
        None => return Ok(()),
    };

    if req.path == "/__brief_web_events" {
        return handle_sse(stream, broadcaster);
    }

    let url_path = strip_query(&req.path);
    let rel = if url_path == "/" || url_path.is_empty() {
        Path::new("index.html").to_path_buf()
    } else {
        Path::new(url_path.trim_start_matches('/')).to_path_buf()
    };
    let mut full = root.join(&rel);
    if full.is_dir() {
        full = full.join("index.html");
    }
    // Prevent traversal — the resolved path must remain under `root`.
    if !is_within(&full, root) {
        return write_simple(&mut stream, 403, "text/plain", b"forbidden");
    }
    if !full.exists() {
        // Try `<path>.html` for clean URLs, else 404.
        let with_html = full.with_extension("html");
        if with_html.exists() {
            return serve_file(&mut stream, &with_html);
        }
        return write_simple(&mut stream, 404, "text/plain", b"not found");
    }
    serve_file(&mut stream, &full)
}

fn handle_sse(mut stream: TcpStream, bcast: &ReloadBroadcaster) -> std::io::Result<()> {
    let headers = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Connection: keep-alive\r\n\
        Access-Control-Allow-Origin: *\r\n\r\n";
    stream.write_all(headers.as_bytes())?;
    stream.flush()?;
    // Initial comment to keep proxies awake.
    stream.write_all(b": connected\n\n")?;
    stream.flush()?;
    let rx = bcast.subscribe();
    // Block on broadcaster events; if the broadcaster drops we exit.
    while let Ok(ev) = rx.recv() {
        match ev {
            ReloadEvent::Reload => {
                if stream.write_all(b"event: reload\ndata: now\n\n").is_err() {
                    break;
                }
            }
            ReloadEvent::BuildError(msg) => {
                let payload = msg.replace('\r', "").replace('\n', " ");
                let line = format!("event: error\ndata: {}\n\n", payload);
                if stream.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
        }
        if stream.flush().is_err() {
            break;
        }
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    #[allow(dead_code)]
    headers: HashMap<String, String>,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    let parts: Vec<&str> = line.trim_end().splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        let m = reader.read_line(&mut header)?;
        if m == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((k, v)) = header.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    if let Some(len) = headers
        .get("content-length")
        .and_then(|s| s.parse::<usize>().ok())
    {
        // Consume request body, if any. We don't actually use it.
        let mut buf = vec![0u8; len.min(1 << 16)];
        let _ = reader.read_exact(&mut buf);
    }
    Ok(Some(Request {
        method,
        path,
        headers,
    }))
}

fn serve_file(stream: &mut TcpStream, path: &Path) -> std::io::Result<()> {
    let bytes = std::fs::read(path)?;
    let mime = mime_for(path);
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        mime,
        bytes.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn write_simple(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        mime,
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()) {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn strip_query(p: &str) -> &str {
    p.split('?').next().unwrap_or(p)
}

fn is_within(child: &Path, parent: &Path) -> bool {
    let cc = match dunce_canon(child) {
        Some(p) => p,
        None => return false,
    };
    let pp = match dunce_canon(parent) {
        Some(p) => p,
        None => return false,
    };
    cc.starts_with(&pp)
}

fn dunce_canon(p: &Path) -> Option<PathBuf> {
    // We avoid pulling in the `dunce` crate; canonicalize is good enough on
    // unix targets and acceptable for a development-only server.
    p.canonicalize().ok().or_else(|| {
        // Walk parents until we find one that exists, then re-attach the tail.
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut cur = p.to_path_buf();
        while !cur.exists() {
            let name = cur.file_name()?.to_os_string();
            tail.push(name);
            cur.pop();
        }
        let mut anchor = cur.canonicalize().ok()?;
        for piece in tail.into_iter().rev() {
            anchor.push(piece);
        }
        Some(anchor)
    })
}

#[allow(dead_code)]
fn _quiet(req: &Request) -> &str {
    &req.method
}

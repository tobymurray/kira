//! A static file server for local development.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use tiny_http::{Header, Response, Server};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/vnd.microsoft.icon",
        _ => "application/octet-stream",
    }
}

/// Resolve a request path inside `root`, rejecting anything that escapes it.
fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path);
    let mut path = root.to_path_buf();
    for component in Path::new(&decoded).components() {
        match component {
            Component::Normal(part) => path.push(part),
            // Traversal and absolute paths are simply not honoured.
            Component::ParentDir => return None,
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if path.is_dir() {
        path.push("index.html");
    }
    Some(path)
}

/// Minimal percent-decoding: enough for file names with spaces or `%20`.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or_default();
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Serve `root` on `port` until interrupted.
///
/// # Errors
/// Fails if the port cannot be bound.
pub(crate) fn run(root: &Path, port: u16) -> Result<()> {
    let root =
        fs::canonicalize(root).with_context(|| format!("no such directory: {}", root.display()))?;
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| anyhow::anyhow!("cannot listen on port {port}: {e}"))?;
    println!(
        "Kira dev server: http://localhost:{port}  (serving {})",
        root.display()
    );

    for request in server.incoming_requests() {
        let url = request
            .url()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_owned();
        let response = match resolve(&root, &url) {
            Some(path) if path.is_file() => match fs::read(&path) {
                Ok(bytes) => {
                    let header = Header::from_bytes("content-type", content_type(&path))
                        .expect("static content type is a valid header");
                    Response::from_data(bytes).with_header(header)
                }
                Err(err) => Response::from_string(err.to_string()).with_status_code(500),
            },
            Some(_) => Response::from_string("not found").with_status_code(404),
            None => Response::from_string("forbidden").with_status_code(403),
        };
        if let Err(err) = request.respond(response) {
            eprintln!("response failed: {err}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_paths_inside_the_root() {
        let root = Path::new("/srv/site");
        assert_eq!(
            resolve(root, "/data/catalog.json"),
            Some(PathBuf::from("/srv/site/data/catalog.json"))
        );
    }

    #[test]
    fn refuses_traversal() {
        assert_eq!(resolve(Path::new("/srv/site"), "/../../etc/passwd"), None);
        assert_eq!(resolve(Path::new("/srv/site"), "/data/../../secret"), None);
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("/a%20b.uapp"), "/a b.uapp");
        assert_eq!(percent_decode("/plain"), "/plain");
        // A stray percent is passed through rather than dropped.
        assert_eq!(percent_decode("/100%"), "/100%");
    }

    #[test]
    fn maps_the_content_types_the_site_needs() {
        assert_eq!(content_type(Path::new("x.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("x.uapp")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type(Path::new("x.js")),
            "text/javascript; charset=utf-8"
        );
    }
}

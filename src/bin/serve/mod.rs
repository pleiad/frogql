//! `frogql --serve <db.gdb>`: the browser explorer, over HTTP, with the
//! database already loaded.
//!
//! The explorer works from a file picker and a static host, which is
//! enough to use it and awkward to *share*: the reader has to be told
//! which file to choose, and a `.gdb` with its `.ltj` sidecar is two.
//! Serving them alongside the page turns that into a URL.
//!
//! **No dependency.** This is HTTP/1.1 over `std::net::TcpListener`,
//! about a hundred lines, because the alternative is putting a web server
//! in the dependency tree of a database CLI — which the Python wheel and
//! the WASM build would then have to be kept clear of, the way they are
//! kept clear of `rustyline` and `ureq`. A static file server for
//! localhost is small enough that writing it costs less than gating it.
//!
//! **Localhost only.** It binds `127.0.0.1`, serves a fixed set of paths,
//! and never interprets the request path as a filesystem path. It is a
//! convenience for looking at your own database, not a deployment target.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

/// The page and its worker are compiled in: they are plain files in the
/// repository, so embedding them costs no build step and guarantees the
/// page matches the binary serving it.
const INDEX_HTML: &str = include_str!("../../../explorer/index.html");
const WORKER_JS: &str = include_str!("../../../explorer/worker.js");

/// Where the generated wasm package might be. It is *not* embedded: it is
/// produced by `wasm-pack`, and making a `cargo build` of the CLI depend
/// on that would break `cargo install` and the release build, neither of
/// which has a wasm toolchain.
fn find_pkg(explicit: Option<&Path>) -> Option<PathBuf> {
    let mut tries: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        tries.push(p.to_path_buf());
    }
    if let Ok(p) = std::env::var("FROGQL_EXPLORER_PKG") {
        tries.push(PathBuf::from(p));
    }
    tries.push(PathBuf::from("explorer/pkg"));
    tries.push(PathBuf::from("pkg"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            tries.push(dir.join("explorer-pkg"));
        }
    }
    tries
        .into_iter()
        .find(|p| p.join("frogql_wasm_bg.wasm").is_file())
}

pub fn serve(db: &Path, port: u16, pkg_hint: Option<&Path>) -> std::io::Result<()> {
    let Some(pkg) = find_pkg(pkg_hint) else {
        eprintln!(
            "error: the explorer's wasm package was not found.\n\
             \n\
             It is built separately, because a `cargo build` of this CLI cannot\n\
             depend on a wasm toolchain. From the repository root:\n\
             \n    cargo install wasm-pack        # once\n\
             \n    wasm-pack build wasm --target web --out-dir ../explorer/pkg\n\
             \n\
             Then re-run, or point at it with --explorer-pkg <dir> or\n\
             FROGQL_EXPLORER_PKG."
        );
        std::process::exit(2);
    };

    let ltj = db.with_extension(
        db.extension()
            .map(|e| format!("{}.ltj", e.to_string_lossy()))
            .unwrap_or_else(|| "ltj".into()),
    );
    let has_ltj = ltj.is_file();

    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    let name = db
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let size = fs::metadata(db).map(|m| m.len()).unwrap_or(0);

    println!("froGQL explorer  http://127.0.0.1:{port}");
    println!(
        "  serving {name} ({:.1} MB){}",
        size as f64 / 1e6,
        if has_ltj { " + .ltj" } else { "" }
    );
    println!("  page from this binary, wasm from {}", pkg.display());
    println!("  Ctrl-C to stop");

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        // One at a time. A browser opens a handful of connections for one
        // page and nobody else is on this port; a thread pool here would
        // be machinery in search of a problem.
        if let Err(e) = handle(stream, db, &ltj, has_ltj, &pkg, &name) {
            eprintln!("  request failed: {e}");
        }
    }
    Ok(())
}

fn handle(
    mut s: TcpStream,
    db: &Path,
    ltj: &Path,
    has_ltj: bool,
    pkg: &Path,
    name: &str,
) -> std::io::Result<()> {
    let mut line = String::new();
    BufReader::new(s.try_clone()?).read_line(&mut line)?;
    let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let path = path.split('?').next().unwrap_or("/");

    match path {
        "/" | "/index.html" => text(&mut s, "text/html; charset=utf-8", INDEX_HTML.as_bytes()),
        "/worker.js" => text(
            &mut s,
            "text/javascript; charset=utf-8",
            WORKER_JS.as_bytes(),
        ),
        // What the page asks for first: whether a database is being served
        // and whether its sidecar came with it.
        "/manifest.json" => {
            let body = format!(
                "{{\"name\":{:?},\"ltj\":{}}}",
                name,
                if has_ltj { "true" } else { "false" }
            );
            text(&mut s, "application/json", body.as_bytes())
        }
        "/db" => file(&mut s, db, "application/octet-stream"),
        "/db.ltj" if has_ltj => file(&mut s, ltj, "application/octet-stream"),
        p if p.starts_with("/pkg/") => {
            // Only the flat contents of the package directory, by file
            // name. The request path is never joined as a path, so `..`
            // is not a traversal — it is simply a name that does not
            // exist.
            let leaf = &p["/pkg/".len()..];
            if leaf.is_empty() || leaf.contains('/') || leaf.contains("..") {
                return not_found(&mut s);
            }
            let ct = if leaf.ends_with(".wasm") {
                "application/wasm"
            } else if leaf.ends_with(".js") {
                "text/javascript; charset=utf-8"
            } else {
                "application/octet-stream"
            };
            file(&mut s, &pkg.join(leaf), ct)
        }
        _ => not_found(&mut s),
    }
}

fn text(s: &mut TcpStream, ct: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)
}

fn file(s: &mut TcpStream, p: &Path, ct: &str) -> std::io::Result<()> {
    let Ok(mut f) = fs::File::open(p) else {
        return not_found(s);
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    )?;
    // Streamed in chunks: a `.gdb` can be hundreds of megabytes and
    // reading it into memory to hand it to a socket would double the
    // resident cost of serving it.
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        s.write_all(&buf[..n])?;
    }
    Ok(())
}

fn not_found(s: &mut TcpStream) -> std::io::Result<()> {
    s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

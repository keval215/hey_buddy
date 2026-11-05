//! The tool against a loopback fake server only (no real network).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use buddy_core::download::Downloader;
use sha2::{Digest, Sha256};

fn registry_path() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../models/registry.json").to_string()
}

/// Serves `body` to every request; counts requests.
fn serve(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            h.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(&body);
        }
    });
    (format!("http://127.0.0.1:{port}/models/m.bin"), hits)
}

fn run(args: &[&str], dl: &Downloader) -> (i32, String, String) {
    let mut args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    args.extend(["--registry".to_string(), registry_path()]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = registry_verify::main_with(&args, dl, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

#[test]
fn prints_size_sha_and_hint_and_leaves_registry_untouched() {
    let body: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let want = format!("{:x}", Sha256::digest(&body));
    let (url, hits) = serve(body.clone());
    let reg = registry_path();
    let before = std::fs::read(&reg).unwrap();
    let dl = Downloader::with_loopback_http_for_tests();
    let (code, out, err) = run(&["--id", "whisper-small", "--url", &url], &dl);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("size_bytes: 100000"), "{out}");
    assert!(out.contains(&format!("sha256: {want}")), "{out}");
    assert!(out.contains("licence (registry says): MIT"), "{out}");
    assert!(out.contains("LICENSE"), "{out}");
    assert!(hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(std::fs::read(&reg).unwrap(), before, "registry untouched");
}

#[test]
fn size_cap_is_enforced() {
    let (url, _) = serve(vec![7u8; 5000]);
    let dl = Downloader::with_loopback_http_for_tests();
    let (code, out, err) = run(
        &[
            "--id",
            "whisper-small",
            "--url",
            &url,
            "--max-bytes",
            "1000",
        ],
        &dl,
    );
    assert_eq!(code, 1);
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("too_large"), "{err}");
}

#[test]
fn production_downloader_refuses_http_and_userinfo_without_a_request() {
    let (url, hits) = serve(vec![1u8; 10]);
    let prod = Downloader::new();
    let (code, _, err) = run(&["--id", "whisper-small", "--url", &url], &prod);
    assert_eq!(code, 1);
    assert!(err.contains("invalid_url"), "{err}");
    for bad in [
        "https://user:pw@example.org/m.bin",
        "ftp://example.org/m.bin",
        "file:///etc/hosts",
        "not a url",
    ] {
        let (code, _, err) = run(&["--id", "whisper-small", "--url", bad], &prod);
        assert_eq!(code, 1, "{bad}");
        assert!(err.contains("invalid_url"), "{bad}: {err}");
    }
    // Userinfo is refused even where loopback http is allowed.
    let dl = Downloader::with_loopback_http_for_tests();
    let with_user = url.replace("127.0.0.1", "u:p@127.0.0.1");
    let (code, _, err) = run(&["--id", "whisper-small", "--url", &with_user], &dl);
    assert_eq!(code, 1);
    assert!(err.contains("invalid_url"), "{err}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "no request was made");
}

#[test]
fn unknown_id_and_bad_args_fail_before_any_request() {
    let (url, hits) = serve(vec![1u8; 10]);
    let dl = Downloader::with_loopback_http_for_tests();
    let (code, _, err) = run(&["--id", "nope", "--url", &url], &dl);
    assert_eq!(code, 1);
    assert!(err.contains("unknown_model"), "{err}");
    assert_eq!(run(&["--id", "x"], &dl).0, 2);
    assert_eq!(run(&["--bogus"], &dl).0, 2);
    assert_eq!(
        run(&["--id", "x", "--url", &url, "--max-bytes", "0"], &dl).0,
        2
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

#[test]
fn hint_base_strips_query_and_file() {
    use registry_verify::licence_hint_base as h;
    assert_eq!(
        h("https://h.example/org/repo/resolve/main/m.bin?token=1#x").as_deref(),
        Some("https://h.example/org/repo/resolve/main")
    );
    assert_eq!(h("https://h.example/m.bin"), None);
}

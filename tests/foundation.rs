//! The public foundation surface later verbs build on: the manifest reader,
//! the persistent port-block allocator and the DNS responder.

use multica_sandbox::dns;
use multica_sandbox::manifest::Manifest;
use multica_sandbox::ports::Allocator;
use std::fs;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn scratch(tag: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(format!("foundation-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

const MANIFEST: &str = r#"x-msbx:
  project: signals
  publish:
    pg: 5432
    web: 3000
  env:
    web: [DATABASE_URL]
  secrets:
    web: [SMTP_PASSWORD]
services:
  pg:
    image: postgres:17
  web:
    build: .
"#;

#[test]
fn reads_a_manifest_from_a_project_checkout() {
    let root = scratch("manifest");
    let dir = root.join(".multica");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("compose.yaml"), MANIFEST).unwrap();

    let manifest = Manifest::from_repo(&root).unwrap();
    assert_eq!(manifest.project, "signals");
    assert_eq!(
        manifest.publish,
        vec![("pg".into(), 5432), ("web".into(), 3000)]
    );
    assert_eq!(manifest.env["web"], vec!["DATABASE_URL"]);
    assert_eq!(manifest.secrets["web"], vec!["SMTP_PASSWORD"]);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn an_invalid_manifest_is_an_error_before_anything_starts() {
    let root = scratch("invalid");
    let dir = root.join(".multica");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("compose.yaml"),
        "x-msbx:\n  publish:\n    pg: 5432\nservices:\n  pg: {}\n",
    )
    .unwrap();

    let error = Manifest::from_repo(&root).unwrap_err().to_string();
    assert!(error.contains("x-msbx.project"), "{error}");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn allocator_persists_blocks_across_processes() {
    let root = scratch("ports");
    let path = root.join("state.json");
    let allocator = Allocator::with_range(&path, 41600, 41664);
    let first = allocator.allocate("signals-mir42", 1).unwrap();
    let second = allocator.allocate("hours-mir42", 1).unwrap();
    assert_eq!(first.start, 41600);
    assert_eq!(second.start, 41616);

    // A new allocator over the same file sees the same blocks.
    let reopened = Allocator::with_range(&path, 41600, 41664);
    assert_eq!(reopened.block("signals-mir42").unwrap(), Some(first));
    assert_eq!(reopened.allocate("signals-mir42", 2).unwrap(), first);
    assert_eq!(reopened.release("signals-mir42").unwrap(), Some(first));
    assert_eq!(reopened.block("signals-mir42").unwrap(), None);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_resolver_answers_the_zone_and_refuses_everything_else() {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let address = socket.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = dns::serve_until(&socket, &stop);
        })
    };

    let inside = dns::query(address, "pg.mir42.signals.msbx.test", 1).unwrap();
    assert_eq!(inside.records, vec![Ipv4Addr::LOCALHOST]);
    let outside = dns::query(address, "hours.test", 1).unwrap();
    assert!(outside.nxdomain(), "names outside the zone are NXDOMAIN");

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

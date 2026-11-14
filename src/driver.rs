//! The host-side primitives the init checks and `doctor` share: command capture,
//! macOS version, TCP reachability and the small amount of JSON the
//! `container` CLI returns.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default)]
pub struct Output {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn text(&self) -> String {
        let mut text = self.stdout.trim().to_string();
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push_str(" | ");
            }
            text.push_str(stderr);
        }
        text
    }
}

/// Run a program, capturing stdout and stderr. A missing binary is a failed
/// output, never a panic.
pub fn run(program: &str, args: &[&str]) -> Output {
    match Command::new(program).args(args).output() {
        Ok(output) => Output {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        },
        Err(error) => Output {
            ok: false,
            stdout: String::new(),
            stderr: error.to_string(),
        },
    }
}

/// Is `program` on PATH?
pub fn has(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program}"))
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// The macOS major version, or `None` when `sw_vers` is unavailable.
pub fn macos_major() -> Option<u32> {
    let out = run("sw_vers", &["-productVersion"]);
    if !out.ok {
        return None;
    }
    out.stdout.trim().split('.').next()?.parse().ok()
}

pub fn macos_version() -> String {
    run("sw_vers", &["-productVersion"])
        .stdout
        .trim()
        .to_string()
}

/// Block until `address` accepts a TCP connection, or the deadline passes.
pub fn wait_for_tcp(address: SocketAddr, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(500));
    }
}

/// The first string value for `key` in a JSON document. Intentionally small:
/// the init checks read one address out of `container inspect` and nothing else.
pub fn json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = json.find(&needle)? + needle.len();
    let rest = json[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Accept `192.168.64.3` and `192.168.64.3/24`.
pub fn ipv4_of(value: &str) -> Option<Ipv4Addr> {
    value.split('/').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_pretty_printed_field() {
        let json = r#"{
            "status": { "networks": [ { "ipv4Address" : "192.168.64.7/24" } ] }
        }"#;
        assert_eq!(
            json_string(json, "ipv4Address").as_deref(),
            Some("192.168.64.7/24")
        );
    }

    #[test]
    fn missing_field_is_none() {
        assert_eq!(json_string("{}", "ipv4Address"), None);
    }

    #[test]
    fn strips_the_prefix_length() {
        assert_eq!(
            ipv4_of("192.168.64.7/24"),
            Some(Ipv4Addr::new(192, 168, 64, 7))
        );
    }

    #[test]
    fn rejects_a_mac_address() {
        assert_eq!(ipv4_of("aa:bb:cc:dd:ee:ff"), None);
    }
}

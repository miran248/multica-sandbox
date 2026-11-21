//! The host-side primitives `doctor`, `init` and the later verbs share: command
//! capture, version parsing, terminal attachment and TCP reachability.

use std::io::IsTerminal;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
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
    run_command(Command::new(program).args(args))
}

/// Run a program in `dir`, capturing stdout and stderr.
pub fn run_in(dir: &Path, program: &str, args: &[&str]) -> Output {
    run_command(Command::new(program).args(args).current_dir(dir))
}

/// Run a program with arguments assembled at runtime.
pub fn run_owned(program: &str, args: &[String]) -> Output {
    run_command(Command::new(program).args(args))
}

fn run_command(command: &mut Command) -> Output {
    match command.output() {
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

/// Can this process prompt for a password? `init` runs `sudo` only when it can.
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Run attached to the terminal: `sudo` needs it. Returns whether the child
/// succeeded.
pub fn run_attached(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map(|status| status.success())
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
        sleep(Duration::from_millis(250));
    }
}

/// The Docker Engine version reported by the daemon.
pub fn engine_version() -> Result<String, String> {
    let out = run("docker", &["version", "--format", "{{.Server.Version}}"]);
    let version = out.stdout.trim();
    if out.ok && !version.is_empty() {
        Ok(version.to_string())
    } else {
        Err(first_line(&out).unwrap_or_else(|| "docker version failed".to_string()))
    }
}

/// The Compose v2 plugin version, without the leading `v`.
pub fn compose_version() -> Result<String, String> {
    let out = run("docker", &["compose", "version", "--short"]);
    let version = out.stdout.trim().trim_start_matches('v');
    if out.ok && !version.is_empty() {
        Ok(version.to_string())
    } else {
        Err(first_line(&out).unwrap_or_else(|| "docker compose version failed".to_string()))
    }
}

/// `Some(server version)` when the Docker daemon answers.
pub fn daemon_version() -> Option<String> {
    let out = run("docker", &["info", "--format", "{{.ServerVersion}}"]);
    let version = out.stdout.trim();
    (out.ok && !version.is_empty()).then(|| version.to_string())
}

/// Is a dotted version at least `major.minor`? `28.2.2-desktop.1` counts as
/// 28.2; a non-numeric prefix parses as 0.
pub fn version_at_least(version: &str, major: u32, minor: u32) -> bool {
    let version = version.trim().trim_start_matches('v');
    let mut parts = version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let found_major = parts.next().unwrap_or(0);
    let found_minor = parts.next().unwrap_or(0);
    (found_major, found_minor) >= (major, minor)
}

/// The first IPv4 address in `text`, ignoring ports and CIDR suffixes.
pub fn first_ipv4(text: &str) -> Option<Ipv4Addr> {
    text.split_whitespace().find_map(|token| {
        let token = token.split('/').next()?.trim();
        token.parse().ok()
    })
}

fn first_line(out: &Output) -> Option<String> {
    out.text()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(version_at_least("28.2.2", 24, 0));
        assert!(version_at_least("28.2.2-desktop.1", 24, 0));
        assert!(version_at_least("2.36.2", 2, 24));
        assert!(!version_at_least("2.20.0", 2, 24));
        assert!(!version_at_least("23.0.6", 24, 0));
        assert!(!version_at_least("", 1, 0));
    }

    #[test]
    fn parses_the_first_address() {
        assert_eq!(first_ipv4("172.26.0.3"), Some(Ipv4Addr::new(172, 26, 0, 3)));
        assert_eq!(
            first_ipv4("192.168.64.7/24"),
            Some(Ipv4Addr::new(192, 168, 64, 7))
        );
        assert_eq!(first_ipv4("no address"), None);
    }
}

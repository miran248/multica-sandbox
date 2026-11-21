//! What the host has to provide and how `init` installs it: the Docker engine
//! is checked elsewhere; here live the `msbx.test` resolver file, the daemon
//! unit, the privileged command, and the leftovers from the dnsmasq era.
//!
//! macOS: `/etc/resolver/msbx.test` plus a LaunchAgent `test.msbx.dns`.
//! Linux: a `systemd-resolved` drop-in plus a `systemd --user` unit.
//!
//! Every install step is idempotent. When the process cannot prompt for a
//! password, the one privileged command is returned in [`Setup::privileged`]
//! and C1 fails with it, rather than pretending the resolver is installed.

use crate::dns;
use crate::driver;
use crate::state;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The only local zone.
pub const ZONE: &str = dns::ZONE;
/// The resolver port.
pub const DNS_PORT: u16 = 5335;
/// The resolver socket.
pub const DNS_ADDR: &str = "127.0.0.1:5335";
/// The launchd label and the systemd unit name.
const DNS_LABEL: &str = "test.msbx.dns";
const DNS_UNIT: &str = "msbx-dns.service";
const RESOLVER_PRINTF: &str = "nameserver 127.0.0.1\\nport 5335\\n";
const RESOLVED_PRINTF: &str = "[Resolve]\\nDNS=127.0.0.1:5335\\nDomains=~msbx.test\\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Other,
}

pub fn os() -> Os {
    if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(target_os = "linux") {
        Os::Linux
    } else {
        Os::Other
    }
}

/// One line of the setup conversation `init` prints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub label: Label,
    pub subject: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    Ok,
    Run,
    Skip,
    Fail,
}

impl Step {
    fn ok(subject: &'static str, detail: impl Into<String>) -> Self {
        Step {
            label: Label::Ok,
            subject,
            detail: detail.into(),
        }
    }

    fn skip(subject: &'static str, detail: impl Into<String>) -> Self {
        Step {
            label: Label::Skip,
            subject,
            detail: detail.into(),
        }
    }

    fn fail(subject: &'static str, detail: impl Into<String>) -> Self {
        Step {
            label: Label::Fail,
            subject,
            detail: detail.into(),
        }
    }
}

/// The result of an install attempt.
#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub steps: Vec<Step>,
    /// The single privileged command still needed, when sudo was unavailable.
    pub privileged: Option<String>,
}

impl Setup {
    fn push(&mut self, step: Step) {
        self.steps.push(step);
    }
}

/// The resolver file the platform uses.
pub fn resolver_file() -> PathBuf {
    match os() {
        Os::MacOs => PathBuf::from("/etc/resolver/msbx.test"),
        Os::Linux => PathBuf::from("/etc/systemd/resolved.conf.d/msbx.conf"),
        Os::Other => PathBuf::from("/etc/resolver/msbx.test"),
    }
}

/// Is the `msbx.test` resolver installed and pointed at the daemon?
pub fn resolver_installed() -> bool {
    let text = fs::read_to_string(resolver_file()).unwrap_or_default();
    match os() {
        Os::MacOs => {
            text.contains("nameserver 127.0.0.1") && text.contains(&format!("port {DNS_PORT}"))
        }
        Os::Linux => {
            text.contains(&format!("DNS=127.0.0.1:{DNS_PORT}"))
                && text.contains(&format!("Domains=~{ZONE}"))
        }
        Os::Other => false,
    }
}

/// The per-user daemon unit path, when the platform has one.
pub fn dns_unit_file() -> Option<PathBuf> {
    match os() {
        Os::MacOs => Some(
            state::home()
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{DNS_LABEL}.plist")),
        ),
        Os::Linux => Some(
            state::home()
                .join(".config")
                .join("systemd")
                .join("user")
                .join(DNS_UNIT),
        ),
        Os::Other => None,
    }
}

pub fn dns_unit_installed() -> bool {
    dns_unit_file().is_some_and(|path| path.exists())
}

/// Is the installed unit running? `None` when the platform cannot say.
pub fn dns_unit_active() -> Option<bool> {
    match os() {
        Os::MacOs => Some(
            driver::run(
                "launchctl",
                &["print", &format!("gui/{}/{DNS_LABEL}", uid())],
            )
            .ok,
        ),
        Os::Linux => Some(
            driver::run("systemctl", &["--user", "is-active", DNS_UNIT])
                .stdout
                .trim()
                == "active",
        ),
        Os::Other => None,
    }
}

/// Resolver files and zones left over from the dnsmasq / per-project-IP era.
pub fn leftovers() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir("/etc/resolver") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name != ZONE {
                out.push(format!("/etc/resolver/{name}"));
            }
        }
    }
    if let Ok(entries) = fs::read_dir("/etc/systemd/resolved.conf.d") {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == "msbx.conf") {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if text.contains("hours.test") || text.contains("signals.test") {
                    out.push(path.display().to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Is a dnsmasq process still running?
pub fn dnsmasq_running() -> bool {
    driver::run("pgrep", &["-x", "dnsmasq"]).ok
}

/// Ask the host's normal resolver for `name`.
pub fn system_resolve(name: &str) -> Option<Ipv4Addr> {
    match os() {
        Os::MacOs => {
            let out = driver::run("dscacheutil", &["-q", "host", "-a", "name", name]);
            out.stdout.lines().find_map(|line| {
                let value = line.trim().strip_prefix("ip_address:")?;
                value.trim().parse().ok()
            })
        }
        Os::Linux => {
            let out = driver::run("getent", &["hosts", name]);
            driver::first_ipv4(&out.stdout)
        }
        Os::Other => None,
    }
}

/// Install the resolver and the daemon unit. No-op steps say so; nothing is
/// rewritten when it is already correct.
pub fn install(exe: &Path) -> Setup {
    let mut setup = Setup::default();
    match os() {
        Os::MacOs => install_macos(exe, &mut setup),
        Os::Linux => install_linux(exe, &mut setup),
        Os::Other => setup.push(Step::fail(
            "platform",
            "only macOS and Linux are supported; install the resolver by hand",
        )),
    }
    setup
}

fn install_macos(exe: &Path, setup: &mut Setup) {
    let destination = resolver_file();
    if resolver_installed() {
        setup.push(Step::ok(
            "resolver",
            format!("{ZONE} already installed at {}", destination.display()),
        ));
    } else {
        let command = privileged_command();
        if driver::is_interactive() {
            let script = macos_resolver_script();
            let installed = driver::run_attached("sudo", &["sh", "-c", &script]);
            if installed && resolver_installed() {
                setup.push(Step::ok(
                    "resolver",
                    format!("{ZONE} installed at {}", destination.display()),
                ));
            } else {
                setup.privileged = Some(command.clone());
                setup.push(Step::fail(
                    "resolver",
                    format!("not installed; run: {command}"),
                ));
            }
        } else {
            setup.privileged = Some(command.clone());
            setup.push(Step::skip(
                "resolver",
                format!("missing and no terminal for sudo; run: {command}"),
            ));
        }
    }

    install_launchd(exe, setup);
    wait_for_daemon(setup);
}

fn install_launchd(exe: &Path, setup: &mut Setup) {
    let Some(unit) = dns_unit_file() else {
        return;
    };
    let log = state::home()
        .join("Library")
        .join("Logs")
        .join("msbx-dns.log");
    let plist = launchd_plist(exe, &log);
    if let Some(parent) = unit.parent() {
        if let Err(error) = state::create_private_dir(parent) {
            setup.push(Step::fail(
                "dns unit",
                format!("cannot create {}: {error}", parent.display()),
            ));
            return;
        }
    }
    if let Err(error) = fs::write(&unit, plist) {
        setup.push(Step::fail(
            "dns unit",
            format!("cannot write {}: {error}", unit.display()),
        ));
        return;
    }

    let _ = driver::run(
        "launchctl",
        &["bootout", &format!("gui/{}/{DNS_LABEL}", uid())],
    );
    let unit_arg = unit.to_string_lossy().to_string();
    let domain = format!("gui/{}", uid());
    let mut loaded = driver::run("launchctl", &["bootstrap", &domain, &unit_arg]).ok;
    if !loaded {
        loaded = driver::run("launchctl", &["load", "-w", &unit_arg]).ok;
    }
    if loaded {
        setup.push(Step::ok(
            "dns unit",
            format!("{DNS_LABEL} loaded from {}", unit.display()),
        ));
    } else {
        setup.push(Step::fail(
            "dns unit",
            format!(
                "could not load {}; run: launchctl bootstrap {domain} {}",
                unit.display(),
                unit.display()
            ),
        ));
    }
}

fn install_linux(exe: &Path, setup: &mut Setup) {
    let destination = resolver_file();
    if resolver_installed() {
        setup.push(Step::ok(
            "resolver",
            format!("{ZONE} already installed at {}", destination.display()),
        ));
    } else {
        let command = privileged_command();
        if driver::is_interactive() {
            let script = linux_resolver_script();
            let installed = driver::run_attached("sudo", &["sh", "-c", &script]);
            let restarted = installed
                && driver::run_attached("sudo", &["systemctl", "restart", "systemd-resolved"]);
            if restarted && resolver_installed() {
                setup.push(Step::ok(
                    "resolver",
                    format!("{ZONE} installed at {}", destination.display()),
                ));
            } else {
                setup.privileged = Some(command.clone());
                setup.push(Step::fail(
                    "resolver",
                    format!("not installed; run: {command}"),
                ));
            }
        } else {
            setup.privileged = Some(command.clone());
            setup.push(Step::skip(
                "resolver",
                format!("missing and no terminal for sudo; run: {command}"),
            ));
        }
    }

    let resolved_active = driver::run("systemctl", &["is-active", "systemd-resolved"])
        .stdout
        .trim()
        == "active";
    if resolver_installed() && !resolved_active {
        setup.push(Step::fail(
            "resolver",
            "the drop-in is installed but systemd-resolved is not active",
        ));
    }

    install_systemd_user(exe, setup);
    wait_for_daemon(setup);
}

fn install_systemd_user(exe: &Path, setup: &mut Setup) {
    let Some(unit) = dns_unit_file() else {
        return;
    };
    if let Some(parent) = unit.parent() {
        if let Err(error) = state::create_private_dir(parent) {
            setup.push(Step::fail(
                "dns unit",
                format!("cannot create {}: {error}", parent.display()),
            ));
            return;
        }
    }
    if let Err(error) = fs::write(&unit, systemd_unit(exe)) {
        setup.push(Step::fail(
            "dns unit",
            format!("cannot write {}: {error}", unit.display()),
        ));
        return;
    }
    let reloaded = driver::run("systemctl", &["--user", "daemon-reload"]).ok;
    let enabled = reloaded && driver::run("systemctl", &["--user", "enable", "--now", DNS_UNIT]).ok;
    if enabled {
        setup.push(Step::ok(
            "dns unit",
            format!("{DNS_UNIT} enabled from {}", unit.display()),
        ));
    } else {
        setup.push(Step::fail(
            "dns unit",
            format!("could not enable {DNS_UNIT}; run: systemctl --user enable --now {DNS_UNIT}"),
        ));
    }
}

/// Wait for the daemon the just-installed unit should have started.
fn wait_for_daemon(setup: &mut Setup) {
    let address: SocketAddr = DNS_ADDR.parse().expect("static resolver address");
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if dns::answers(address) {
            setup.push(Step::ok("dns", format!("{DNS_ADDR} answers for {ZONE}")));
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    setup.push(Step::fail(
        "dns",
        format!("{DNS_ADDR} does not answer; check `msbx dns` and the installed unit"),
    ));
}

/// The one privileged command a non-interactive `init` needs. It is complete:
/// it writes the file and restarts the resolver, with no staging path to keep.
pub fn privileged_command() -> String {
    match os() {
        Os::MacOs => format!("sudo sh -c \"{}\"", macos_resolver_script()),
        Os::Linux => format!(
            "sudo sh -c \"{}\" && sudo systemctl restart systemd-resolved",
            linux_resolver_script()
        ),
        Os::Other => "install a resolver for msbx.test by hand".to_string(),
    }
}

fn macos_resolver_script() -> String {
    format!("printf '{RESOLVER_PRINTF}' > /etc/resolver/{ZONE} && chmod 644 /etc/resolver/{ZONE}")
}

fn linux_resolver_script() -> String {
    format!(
        "mkdir -p /etc/systemd/resolved.conf.d && printf '{RESOLVED_PRINTF}' > /etc/systemd/resolved.conf.d/msbx.conf"
    )
}

fn launchd_plist(exe: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{DNS_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>dns</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = xml_escape(&exe.to_string_lossy()),
        log = xml_escape(&log.to_string_lossy()),
    )
}

fn systemd_unit(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription={ZONE} resolver\n\n[Service]\nExecStart=\"{}\" dns\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn uid() -> String {
    driver::run("id", &["-u"]).stdout.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_the_binary_and_the_verb() {
        let plist = launchd_plist(Path::new("/opt/msbx"), Path::new("/tmp/msbx.log"));
        assert!(plist.contains("<string>test.msbx.dns</string>"));
        assert!(plist.contains("<string>/opt/msbx</string>"));
        assert!(plist.contains("<string>dns</string>"));
    }

    #[test]
    fn the_systemd_unit_runs_the_resolver() {
        let unit = systemd_unit(Path::new("/opt/msbx"));
        assert!(unit.contains("ExecStart=\"/opt/msbx\" dns"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn escapes_xml_metacharacters() {
        assert_eq!(xml_escape("a&b<c>"), "a&amp;b&lt;c&gt;");
    }

    #[test]
    fn the_privileged_command_needs_no_staging_file() {
        let command = privileged_command();
        if os() != Os::Other {
            assert!(command.starts_with("sudo sh -c \""));
            assert!(command.contains("127.0.0.1"));
        }
    }

    #[test]
    fn the_resolver_content_is_the_documented_one() {
        assert!(RESOLVER_PRINTF.contains("nameserver 127.0.0.1"));
        assert!(RESOLVER_PRINTF.contains("port 5335"));
        assert!(RESOLVED_PRINTF.contains("DNS=127.0.0.1:5335"));
        assert!(RESOLVED_PRINTF.contains("Domains=~msbx.test"));
    }
}

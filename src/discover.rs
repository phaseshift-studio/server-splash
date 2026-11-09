//! Deterministic host discovery.
//!
//! The original implementation asked an LLM to read `systemctl` and `docker ps`
//! output and guess what was running. That is slow, non-deterministic, and blind to
//! anything that is not a systemd service or a published container port — which is
//! most hand-started services on a homelab box.
//!
//! This module answers the question directly, from four independent sources:
//!
//!   * listening TCP sockets (`ss`, falling back to `/proc/net/tcp*`) — ground truth
//!     for "is something serving on this port *right now*"
//!   * systemd units — gives a nice human description for a port's process
//!   * docker containers — gives a name and image for published ports
//!   * `nvidia-smi` — GPU inventory for the page subtitle
//!
//! Every parser is a pure function over captured command output so it can be unit
//! tested without the corresponding daemon present.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

/// One GPU as reported by `nvidia-smi`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Gpu {
    pub name: String,
    pub mem_pct: u32,
    pub temp_c: Option<i64>,
    pub fan_pct: Option<i64>,
    pub util_pct: Option<i64>,
}

/// A physical disk, from `lsblk`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Disk {
    pub name: String,
    pub size_bytes: u64,
}

/// A network interface with a negotiated link speed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NetIface {
    pub name: String,
    pub speed_mbps: i64,
    pub state: String,
    /// A real device rather than a bridge, veth pair or tunnel.
    pub physical: bool,
}

/// Smoothed load averages from `/proc/loadavg`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LoadAverage {
    pub one: f32,
    pub five: f32,
    pub fifteen: f32,
}

/// Memory usage from `/proc/meminfo`, in mebibytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MemInfo {
    pub total_mb: u64,
    /// `MemTotal - MemAvailable`: what system monitors call "used", counting
    /// reclaimable page cache as free rather than as a leak.
    pub used_mb: u64,
    pub available_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
}

/// Machine load sampled at generation time, for the panel beside the GPU cards.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SystemStats {
    pub load: Option<LoadAverage>,
    /// Busy CPU percentage over a short sample, not an average since boot.
    pub cpu_pct: Option<u32>,
    pub mem: Option<MemInfo>,
    /// Logical cores. Load is only meaningful per core, so the cards qualify it.
    pub cpu_count: usize,
}

impl SystemStats {
    pub(crate) fn is_empty(&self) -> bool {
        self.load.is_none() && self.cpu_pct.is_none() && self.mem.is_none()
    }

    /// Memory in use as a percentage, for the big number on the card.
    pub(crate) fn mem_pct(&self) -> Option<u32> {
        let mem = self.mem?;
        if mem.total_mb == 0 {
            return None;
        }
        Some(((mem.used_mb as f64 / mem.total_mb as f64) * 100.0).round().clamp(0.0, 100.0) as u32)
    }
}

/// Static facts about the machine, used for the page subtitle.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Machine {
    pub hostname: String,
    /// Primary routable address, shown next to the hostname in the page footer.
    pub ip: String,
    pub distro: String,
    pub kernel: String,
    pub mem_mb: u64,
    pub cpu_count: usize,
    pub gpus: Vec<Gpu>,
    pub disks: Vec<Disk>,
    pub net: Vec<NetIface>,
    /// Load, CPU and memory sampled at generation time.
    pub system: SystemStats,
}

impl Machine {
    /// `Dual RTX 3090 — Linux Mint 22.3 — services directory`, mirroring the look of
    /// the hand-maintained page this project is replacing.
    pub(crate) fn subtitle(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(gpus) = self.gpu_summary() {
            parts.push(gpus);
        }
        if !self.distro.is_empty() {
            parts.push(self.distro.clone());
        }
        parts.push("services directory".to_string());
        parts.join(" — ")
    }

    /// The little stat line under the title: memory, storage, link speed.
    ///
    /// Each entry is a separate chip on the page, so an unknown fact is simply
    /// absent rather than rendered as "unknown".
    pub(crate) fn stat_chips(&self) -> Vec<String> {
        let mut chips: Vec<String> = Vec::new();

        if self.mem_mb > 0 {
            chips.push(format!("{} RAM", human_bytes(self.mem_mb * 1024 * 1024)));
        }

        if !self.disks.is_empty() {
            let total: u64 = self.disks.iter().map(|d| d.size_bytes).sum();
            let n = self.disks.len();
            chips.push(format!(
                "{} across {} disk{}",
                human_storage(total),
                n,
                if n == 1 { "" } else { "s" }
            ));
        }

        if self.cpu_count > 0 {
            chips.push(format!("{} cores", self.cpu_count));
        }

        // Only interfaces with a real negotiated rate.
        //
        // Physical devices are preferred: a Docker bridge reports 10 Gb/s, which would
        // outrank (and hide) the actual 1 Gb/s NIC the machine is reached over.
        let usable = |i: &&NetIface| i.speed_mbps > 0;
        let physical: Vec<&NetIface> = self.net.iter().filter(|i| i.physical).filter(usable).collect();
        let chosen = if physical.is_empty() {
            self.net.iter().filter(usable).collect::<Vec<_>>()
        } else {
            physical
        };
        for iface in chosen.into_iter().take(2) {
            chips.push(format!("{} {}", iface.name, human_bitrate(iface.speed_mbps)));
        }

        chips
    }

    /// Collapse identical GPUs into `Dual RTX 3090` / `4× RTX 3090` style text.
    pub(crate) fn gpu_summary(&self) -> Option<String> {
        if self.gpus.is_empty() {
            return None;
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        for g in &self.gpus {
            let short = short_gpu_name(&g.name);
            match counts.iter_mut().find(|(n, _)| *n == short) {
                Some((_, c)) => *c += 1,
                None => counts.push((short, 1)),
            }
        }
        let rendered: Vec<String> = counts
            .iter()
            .map(|(name, n)| match n {
                1 => name.clone(),
                2 => format!("Dual {name}"),
                n => format!("{n}× {name}"),
            })
            .collect();
        Some(rendered.join(" + "))
    }
}

/// Render memory as `64 GB`, `512 MB` — binary multiples, which is how RAM is sold
/// and how every other system tool reports it.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    // One decimal below 10 (4.0 TB), none above (64 GB).
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Render storage as `4.0 TB`, `2.0 TB` — decimal multiples.
///
/// Drives are sold in decimal TB, so a binary reading turns a "4 TB" disk into
/// "3.6 TB", which reads like a fault rather than a unit convention.
pub(crate) fn human_storage(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Render a link rate as `1 Gb/s`, `100 Mb/s`, `2.5 Gb/s`.
pub(crate) fn human_bitrate(mbps: i64) -> String {
    if mbps >= 1000 {
        let gb = mbps as f64 / 1000.0;
        if (gb.fract() * 10.0).abs() < f64::EPSILON {
            format!("{gb:.0} Gb/s")
        } else {
            format!("{gb:.1} Gb/s")
        }
    } else {
        format!("{mbps} Mb/s")
    }
}

/// Parse `lsblk -bdno NAME,SIZE,TYPE`.
pub(crate) fn parse_lsblk(output: &str) -> Vec<Disk> {
    let mut out = Vec::new();
    for line in output.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 3 || f[2] != "disk" {
            continue; // partitions, loops, rom devices
        }
        let Ok(size) = f[1].parse::<u64>() else { continue };
        out.push(Disk { name: f[0].to_string(), size_bytes: size });
    }
    out
}

/// Read `/sys/class/net/*/speed` for every interface that reports one.
pub(crate) fn collect_net() -> Vec<NetIface> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else { return out };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        let read = |file: &str| {
            std::fs::read_to_string(entry.path().join(file))
                .ok()
                .map(|v| v.trim().to_string())
        };
        // Virtual interfaces report -1 or an empty file; treat that as "no link rate".
        let speed_mbps = read("speed").and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
        let state = read("operstate").unwrap_or_else(|| "unknown".to_string());
        // Only real devices have a `device` symlink; bridges, veth pairs and tunnels do
        // not. This is what separates `eno1` from `br-0076f5b84c61`.
        let physical = entry.path().join("device").exists();
        out.push(NetIface { name, speed_mbps, state, physical });
    }
    out.sort_by(|a, b| b.speed_mbps.cmp(&a.speed_mbps).then(a.name.cmp(&b.name)));
    out
}

/// Turn a bare hostname into a resolvable one: `ginger` -> `ginger.local`.
///
/// Links on the page are followed by a *browser*, not by the generator, so a bare
/// label only works if the viewer's resolver has the right search domain. Anything
/// already usable is left alone: IP addresses, names that already carry a domain
/// (`ginger.phaseshift.studio`), `localhost`, and IPv6 literals.
pub(crate) fn qualify_host(host: &str, suffix: &str) -> String {
    let host = host.trim();
    if host.is_empty() || suffix.is_empty() {
        return host.to_string();
    }
    if host.contains('.') || host.contains(':') {
        return host.to_string(); // already qualified, an IP, or an IPv6 literal
    }
    if host.eq_ignore_ascii_case("localhost") {
        return host.to_string();
    }
    let suffix = if suffix.starts_with('.') { suffix.to_string() } else { format!(".{suffix}") };
    format!("{host}{suffix}")
}

/// Does this URL point at the viewer's own machine rather than the dashboard's host?
///
/// `http://localhost:61208` in a page served from elsewhere is read by the browser as
/// "the machine displaying this page", which is why GPU stats silently never load.
pub(crate) fn is_viewer_localhost(url: &str) -> bool {
    let rest = url
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let host = rest.split(['/', ':']).next().unwrap_or("");
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host == "::1"
        || host == "[::1]"
}

/// `NVIDIA GeForce RTX 3090` -> `RTX 3090`; leaves anything unrecognised alone.
///
/// Vendor words are dropped token-wise rather than with `trim_start_matches`, which
/// would stop at the first non-match and leave `GeForce RTX 3090` behind.
pub(crate) fn short_gpu_name(full: &str) -> String {
    const NOISE: [&str; 3] = ["nvidia", "geforce", "corporation"];
    let kept: Vec<&str> = full
        .split_whitespace()
        .filter(|t| !NOISE.contains(&t.to_lowercase().as_str()))
        .collect();
    if kept.is_empty() {
        full.trim().to_string()
    } else {
        kept.join(" ")
    }
}

/// An icon the service publishes about itself (usually a favicon).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FetchedIcon {
    pub bytes: Vec<u8>,
    /// File extension without the dot, derived from the content, not the URL.
    pub ext: String,
    /// Where it came from — shown in the manifest for traceability.
    pub source: String,
}

/// What an HTTP probe learned about a port.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HttpInfo {
    pub scheme: String,
    pub status: u16,
    pub server: Option<String>,
    pub title: Option<String>,
    /// The service's own icon, if it publishes one we could read.
    pub icon: Option<FetchedIcon>,
}

/// One service-bearing endpoint found on the host.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiscoveredService {
    pub port: u16,
    /// Socket is bound to a routable address (not just loopback).
    pub remote_reachable: bool,
    pub proc_name: Option<String>,
    pub pid: Option<u32>,
    pub unit: Option<String>,
    pub unit_desc: Option<String>,
    pub container: Option<String>,
    pub image: Option<String>,
    pub http: Option<HttpInfo>,
}

/// Does this look like a container id rather than the name of a machine?
///
/// Docker names a container after its id unless told otherwise, so a dashboard served
/// from one without `uts: host` describes itself as `f3a9c1b2d4e5` and every link on
/// the page points at a name nothing can resolve.
pub(crate) fn looks_like_container_id(hostname: &str) -> bool {
    let name = hostname.trim();
    name.len() == 12 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Is this process name evidence of a container runtime rather than of the service?
///
/// Docker forwards every published port through `docker-proxy`, so a host running ten
/// containers has ten ports all owned by a process named after Docker itself. Using
/// that as an identity names every one of them "Docker" — which is how Dockge came to
/// be labelled as the engine it runs on.
pub(crate) fn is_runtime_plumbing(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(), "docker-proxy" | "containerd-shim")
}

impl DiscoveredService {
    /// The best available single-word identity, most specific source first.
    pub(crate) fn identity(&self) -> String {
        if let Some(c) = &self.container {
            return c.clone();
        }
        if let Some(u) = &self.unit {
            return u.trim_end_matches(".service").to_string();
        }
        if let Some(p) = &self.proc_name {
            if !is_runtime_plumbing(p) {
                return p.clone();
            }
        }
        format!("port-{}", self.port)
    }

    /// Stable curation key, e.g. `docker:zigbee2mqtt:8080` or `port:11434`.
    ///
    /// The port is part of the key on purpose: one container commonly publishes
    /// several ports (a web UI and a VNC socket, say), and keying those on the
    /// container name alone made them collide and drop all but one.
    pub(crate) fn id(&self) -> String {
        if let Some(c) = &self.container {
            return format!("docker:{}:{}", c.to_lowercase(), self.port);
        }
        if let Some(u) = &self.unit {
            return format!("systemd:{}:{}", u.trim_end_matches(".service").to_lowercase(), self.port);
        }
        if let Some(p) = &self.proc_name {
            // Same reasoning as `identity`: a port forwarder must not become the key
            // that curation and de-duplication are built on.
            if !is_runtime_plumbing(p) {
                return format!("proc:{}:{}", p.to_lowercase(), self.port);
            }
        }
        format!("port:{}", self.port)
    }

    /// Human-readable provenance shown in the card tooltip.
    pub(crate) fn source(&self) -> String {
        match (&self.container, &self.unit, &self.proc_name) {
            (Some(c), _, _) => format!("docker container {c}"),
            (None, Some(u), _) => format!("systemd unit {u}"),
            (None, None, Some(p)) => format!("process {p}"),
            _ => format!("listening port {}", self.port),
        }
    }
}

/// Everything discovery learned about the host.
#[derive(Debug, Clone, Default)]
pub(crate) struct Discovery {
    pub machine: Machine,
    pub services: Vec<DiscoveredService>,
}

/// Tunables for a discovery pass.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// Include ports bound to loopback only. Off by default: a link to
    /// `127.0.0.1:631` is useless (or wrong) from another machine.
    pub include_loopback: bool,
    /// Ports never shown, e.g. the web server serving this very page.
    pub exclude_ports: Vec<u16>,
    /// Include the kernel's ephemeral port range. Off by default: those are
    /// short-lived test/container ports that add noise rather than services.
    pub include_ephemeral: bool,
    /// The ephemeral range to exclude, read once from the kernel.
    pub ephemeral: (u16, u16),
    /// Attempt an HTTP probe per port to classify it and pick up its `<title>`.
    pub probe_http: bool,
    pub http_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            include_loopback: false,
            exclude_ports: Vec::new(),
            include_ephemeral: false,
            ephemeral: ephemeral_range(),
            probe_http: true,
            http_timeout: Duration::from_millis(1500),
        }
    }
}

impl Options {
    /// Should this port appear on the dashboard at all?
    ///
    /// The excluded range is the *kernel's* ephemeral range, not "everything above
    /// 32767". Getting that wrong silently dropped Glances, which listens on 61208 —
    /// just above the usual `32768 60999` range, and exactly the kind of long-running
    /// service this page exists to surface.
    pub(crate) fn keep_port(&self, port: u16) -> bool {
        if self.include_ephemeral {
            return true;
        }
        let (low, high) = self.ephemeral;
        !(low..=high).contains(&port)
    }
}

/// The kernel's ephemeral port range, e.g. `32768 60999`.
pub(crate) fn ephemeral_range() -> (u16, u16) {
    /// Linux's `net.ipv4.ip_local_port_range` default.
    const DEFAULT: (u16, u16) = (32768, 60999);
    std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .as_deref()
        .and_then(parse_ephemeral_range)
        .unwrap_or(DEFAULT)
}

/// Parse `32768 60999`.
pub(crate) fn parse_ephemeral_range(text: &str) -> Option<(u16, u16)> {
    let mut fields = text.split_whitespace();
    let low: u16 = fields.next()?.parse().ok()?;
    let high: u16 = fields.next()?.parse().ok()?;
    if low <= high { Some((low, high)) } else { None }
}

/// Run a command, returning stdout, or `None` if it is missing or failed.
fn run_cmd(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ───────────────────────────── parsers ─────────────────────────────

/// Parse `ss -ltnp` output into `(port, remote_reachable, proc_name, pid)`.
///
/// Without `-p` (or without privileges) the process column is simply absent, which
/// is why every process field is optional.
pub(crate) fn parse_ss(output: &str) -> Vec<(u16, bool, Option<String>, Option<u32>)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("State") || line.starts_with("Netid") {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || !fields[0].eq_ignore_ascii_case("LISTEN") {
            continue;
        }
        // `ss -lnt` puts the local address in column 3; `ss -ltnp` adds a process
        // column but never shifts the first four.
        let local = fields[3];
        let Some((addr, port)) = split_host_port(local) else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };

        let remote_reachable = !is_loopback_addr(&addr);
        let (proc_name, pid) = parse_ss_process(line);
        out.push((port, remote_reachable, proc_name, pid));
    }
    out
}

/// Split `0.0.0.0:80`, `[::]:22`, `*:8080` or `127.0.0.1:631` into address and port.
fn split_host_port(s: &str) -> Option<(String, String)> {
    let (addr, port) = s.rsplit_once(':')?;
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let addr = addr.trim_start_matches('[').trim_end_matches(']');
    Some((addr.to_string(), port.to_string()))
}

fn is_loopback_addr(addr: &str) -> bool {
    addr == "127.0.0.1"
        || addr == "::1"
        || addr.starts_with("127.")
        || addr.eq_ignore_ascii_case("localhost")
}

/// Pull `users:(("apache2",pid=1234,fd=4))` out of an `ss -p` line.
fn parse_ss_process(line: &str) -> (Option<String>, Option<u32>) {
    let Some(idx) = line.find("users:((") else { return (None, None) };
    let rest = &line[idx + "users:((".len()..];
    // rest is `"apache2",pid=1234,fd=4))` — the name sits between the first pair of quotes.
    let name = rest
        .strip_prefix('"')
        .and_then(|s| s.split('"').next())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let pid = rest
        .split("pid=")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.parse::<u32>().ok());
    (name, pid)
}

/// Parse `docker ps --format '{{json .}}'` output (one JSON object per line).
///
/// Returns `(names, image, published_host_ports)`.
pub(crate) fn parse_docker_json(output: &str) -> Vec<(String, String, Vec<u16>)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let names = v.get("Names").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let image = v.get("Image").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let ports = v.get("Ports").and_then(|x| x.as_str()).unwrap_or("");
        out.push((names, image, parse_published_ports(ports)));
    }
    out
}

/// Extract host ports from `0.0.0.0:8080->80/tcp, [::]:8080->80/tcp`.
pub(crate) fn parse_published_ports(s: &str) -> Vec<u16> {
    let mut out = Vec::new();
    for chunk in s.split(',') {
        let chunk = chunk.trim();
        // Ignore `80/tcp` (exposed but not published) — only `->` mappings are reachable.
        let Some((host_side, _)) = chunk.split_once("->") else { continue };
        let Some((_, port)) = host_side.rsplit_once(':') else { continue };
        if let Ok(p) = port.trim().parse::<u16>() {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Parse `systemctl list-units --type=service --no-legend --plain` output into
/// `(unit, description)`.
pub(crate) fn parse_systemd_units(output: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim_start_matches(['●', ' ', '*']);
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 || !fields[0].contains('.') {
            continue;
        }
        let unit = fields[0].to_string();
        // `unit load active sub Description words…`
        let desc = fields[4..].join(" ");
        out.push((unit, desc));
    }
    out
}

/// Parse `nvidia-smi --query-gpu=name,memory.total,memory.used,temperature.gpu,fan.speed,utilization.gpu`
/// with `--format=csv,noheader,nounits`.
pub(crate) fn parse_nvidia_smi(output: &str) -> Vec<Gpu> {
    let mut out = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.contains("No devices") {
            continue;
        }
        let cols: Vec<&str> = line.split(',').map(|c| c.trim()).collect();
        if cols.len() < 2 {
            continue;
        }
        let total = cols[1].parse::<f64>().ok().filter(|v| *v > 0.0);
        let used = cols.get(2).and_then(|c| c.parse::<f64>().ok());
        let mem_pct = match (total, used) {
            (Some(t), Some(u)) => ((u / t) * 100.0).round().clamp(0.0, 100.0) as u32,
            _ => 0,
        };
        out.push(Gpu {
            name: cols[0].to_string(),
            mem_pct,
            temp_c: cols.get(3).and_then(|c| c.parse::<i64>().ok()),
            fan_pct: cols.get(4).and_then(|c| c.parse::<i64>().ok()),
            util_pct: cols.get(5).and_then(|c| c.parse::<i64>().ok()),
        });
    }
    out
}

/// Parse `/proc/net/tcp` style state files for LISTEN sockets (hex port, little-endian).
fn parse_proc_net_tcp(content: &str) -> Vec<(u16, bool, Option<String>, Option<u32>)> {
    let mut out = Vec::new();
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        // st == 0A is TCP_LISTEN
        if fields[3] != "0A" {
            continue;
        }
        let Some((addr_hex, port_hex)) = fields[1].split_once(':') else { continue };
        let Ok(port) = u16::from_str_radix(port_hex, 16) else { continue };
        // IPv4 addresses are stored little-endian: 0100007F -> 127.0.0.1
        let loopback = if addr_hex.len() == 8 {
            u32::from_str_radix(addr_hex, 16).map(|v| v & 0xFF == 127).unwrap_or(false)
        } else {
            // IPv6: all-zero except the final 1 means ::1
            addr_hex.trim_start_matches('0') == "1"
        };
        out.push((port, !loopback, None, None));
    }
    out
}

// ───────────────────────────── probing ─────────────────────────────

/// Ask a port what it is over HTTP. `None` means "not an HTTP(S) server".
pub(crate) fn probe_http(port: u16, timeout: Duration) -> Option<HttpInfo> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .ok()?;

    for scheme in ["http", "https"] {
        let url = format!("{scheme}://127.0.0.1:{port}/");
        let Ok(resp) = client.get(&url).send() else { continue };
        let status = resp.status().as_u16();
        let server = resp
            .headers()
            .get(reqwest::header::SERVER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        // Read only the head of the body — enough for <title>, cheap for big pages.
        let body = resp.text().unwrap_or_default();
        let title = extract_title(&body);
        let base = format!("{scheme}://127.0.0.1:{port}");
        let icon = fetch_icon(&client, &body, &base, timeout);
        return Some(HttpInfo { scheme: scheme.to_string(), status, server, title, icon });
    }
    None
}

/// Pull the first `<title>` out of an HTML document.
pub(crate) fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let start = lower.find("<title")?;
    let after = &html[start..];
    let open_end = after.find('>')? + 1;
    let rest = &after[open_end..];
    let end = rest.to_lowercase().find("</title>")?;
    let title = rest[..end].trim();
    if title.is_empty() {
        None
    } else {
        Some(collapse_ws(title))
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Largest icon we will carry around; anything bigger is not an icon.
const MAX_ICON_BYTES: usize = 128 * 1024;

/// Find the icon a page declares, falling back to the conventional `/favicon.ico`.
///
/// Most services do not answer on `/favicon.ico` (Grafana, Home Assistant and friends
/// declare theirs under a versioned path), which is why the `<link rel="icon">` in the
/// markup is consulted first.
pub(crate) fn fetch_icon(
    client: &reqwest::blocking::Client,
    html: &str,
    base: &str,
    timeout: Duration,
) -> Option<FetchedIcon> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(href) = extract_icon_href(html) {
        candidates.push(resolve_url(base, &href));
    }
    candidates.push(format!("{base}/favicon.ico"));

    for url in candidates {
        // An icon is a nicety; never let one slow a discovery pass down.
        let Ok(resp) = client.get(&url).timeout(timeout.min(Duration::from_millis(800))).send() else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let Ok(bytes) = resp.bytes() else { continue };
        if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
            continue;
        }
        // Trust the bytes, not the URL or the declared type: a missing favicon often
        // comes back as a 200 HTML error page.
        let Some(ext) = sniff_image_ext(&bytes, &content_type) else { continue };
        return Some(FetchedIcon { bytes: bytes.to_vec(), ext, source: url });
    }
    None
}

/// Pull `href` out of the first `<link rel="…icon…">` in the document.
pub(crate) fn extract_icon_href(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let mut from = 0usize;
    while let Some(rel_idx) = lower[from..].find("<link") {
        let start = from + rel_idx;
        let end = lower[start..].find('>').map(|i| start + i).unwrap_or(lower.len());
        let tag = &html[start..end];
        let tag_lower = &lower[start..end];
        if tag_lower.contains("icon") && !tag_lower.contains("apple-touch-icon-precomposed") {
            if let Some(href_start) = tag_lower.find("href=") {
                let after = &tag[href_start + 5..];
                let quote = after.chars().next()?;
                if quote == '"' || quote == '\'' {
                    let rest = &after[1..];
                    if let Some(close) = rest.find(quote) {
                        let href = rest[..close].trim();
                        if !href.is_empty() {
                            return Some(href.to_string());
                        }
                    }
                }
            }
        }
        from = end + 1;
        if from >= lower.len() {
            break;
        }
    }
    None
}

/// Resolve a possibly-relative `href` against a base like `http://127.0.0.1:8080`.
pub(crate) fn resolve_url(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("http");
        return format!("{scheme}://{rest}");
    }
    if href.starts_with('/') {
        return format!("{base}{href}");
    }
    format!("{base}/{href}")
}

/// Identify an image from its leading bytes; the declared content type is only a hint.
pub(crate) fn sniff_image_ext(bytes: &[u8], content_type: &str) -> Option<String> {
    let ext = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "gif"
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        "ico"
    } else if bytes.starts_with(b"BM") {
        "bmp"
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        "webp"
    } else if content_type.contains("svg") && (bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml")) {
        "svg"
    } else {
        return None;
    };
    Some(ext.to_string())
}

// ───────────────────────────── system load ─────────────────────────────

/// Sample `/proc` for load, memory and CPU. A missing file (or a non-Linux host)
/// simply leaves that field absent, so the panel degrades instead of lying.
pub(crate) fn collect_system_stats() -> SystemStats {
    SystemStats {
        load: std::fs::read_to_string("/proc/loadavg")
            .ok()
            .as_deref()
            .and_then(parse_loadavg),
        cpu_pct: sample_cpu_usage(),
        mem: std::fs::read_to_string("/proc/meminfo")
            .ok()
            .as_deref()
            .and_then(parse_meminfo),
        cpu_count: 0,
    }
}

/// Measure CPU busy percentage over a short window.
///
/// `/proc/stat` is a counter since boot, so one read gives an average over the
/// machine's whole uptime — a number that barely moves and hides a busy moment. Two
/// reads a moment apart measure what the machine is doing now, which is the whole
/// point of the card.
fn sample_cpu_usage() -> Option<u32> {
    // Long enough to average out the sampling instant, short enough to be free in a
    // regeneration loop that runs every 15 minutes.
    const SAMPLE: Duration = Duration::from_millis(150);
    let first = parse_cpu_times(&std::fs::read_to_string("/proc/stat").ok()?)?;
    std::thread::sleep(SAMPLE);
    let second = parse_cpu_times(&std::fs::read_to_string("/proc/stat").ok()?)?;
    cpu_usage_pct(&first, &second)
}

/// Aggregate CPU counters from the `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CpuTimes {
    /// `idle + iowait` — time the CPU had nothing to run.
    idle: u64,
    /// Every counter except `guest`/`guest_nice`, which the kernel already includes in
    /// `user`/`nice` and would otherwise be counted twice.
    total: u64,
}

/// Parse the aggregate `cpu` line (not `cpu0`, `cpu1`, …).
pub(crate) fn parse_cpu_times(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    // `guest`/`guest_nice` are already included in `user`/`nice`, so take only the
    // first eight counters. Any malformed field rejects the whole line rather than
    // shifting the idle column.
    let fields: Option<Vec<u64>> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(|f| f.parse::<u64>().ok())
        .collect();
    let fields = fields?;
    if fields.len() < 4 {
        return None;
    }
    let total = fields.iter().sum();
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
    Some(CpuTimes { idle, total })
}

/// Busy CPU ticks out of total ticks between two samples, as a percentage.
pub(crate) fn cpu_usage_pct(first: &CpuTimes, second: &CpuTimes) -> Option<u32> {
    let total = second.total.checked_sub(first.total)?;
    let idle = second.idle.checked_sub(first.idle)?;
    if total == 0 {
        return None;
    }
    let busy = total.saturating_sub(idle);
    Some(((busy as f64 / total as f64) * 100.0).round().clamp(0.0, 100.0) as u32)
}

/// `2.83 2.66 2.46 2/3621 334586` -> the three load averages.
pub(crate) fn parse_loadavg(text: &str) -> Option<LoadAverage> {
    let mut fields = text.split_whitespace();
    let one: f32 = fields.next()?.parse().ok()?;
    let five: f32 = fields.next()?.parse().ok()?;
    let fifteen: f32 = fields.next()?.parse().ok()?;
    Some(LoadAverage { one, five, fifteen })
}

/// Parse `/proc/meminfo` into mebibytes.
///
/// `MemAvailable` is what the kernel offers a new workload and is the right basis for
/// "used": it excludes reclaimable cache, so a machine that has been up for a month
/// does not look full. Older kernels (before 3.14) lack it, so fall back to the
/// free + buffers + cached sum that the field replaced.
pub(crate) fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut values: BTreeMap<&str, u64> = BTreeMap::new();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else { continue };
        let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        values.insert(key.trim(), kb / 1024);
    }

    let total_mb = *values.get("MemTotal")?;
    if total_mb == 0 {
        return None;
    }
    let available_mb = values.get("MemAvailable").copied().unwrap_or_else(|| {
        values.get("MemFree").copied().unwrap_or(0)
            + values.get("Buffers").copied().unwrap_or(0)
            + values.get("Cached").copied().unwrap_or(0)
    });

    let swap_total_mb = values.get("SwapTotal").copied().unwrap_or(0);
    let swap_free_mb = values.get("SwapFree").copied().unwrap_or(swap_total_mb);

    Some(MemInfo {
        total_mb,
        used_mb: total_mb.saturating_sub(available_mb),
        available_mb,
        swap_total_mb,
        swap_used_mb: swap_total_mb.saturating_sub(swap_free_mb),
    })
}

// ───────────────────────────── top level ─────────────────────────────

pub(crate) fn collect_machine() -> Machine {
    // The `hostname` command reports the UTS namespace, which is authoritative. With
    // `uts: host` in a container that is the real machine; `/etc/hostname` is a
    // container-local file and would report the image's or the container id.
    let hostname = run_cmd("hostname", &[])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "localhost".to_string());

    let distro = collect_distro();

    let kernel = run_cmd("uname", &["-r"]).map(|s| s.trim().to_string()).unwrap_or_default();

    let mut system = collect_system_stats();
    // The chips show total RAM; reuse the sample rather than reading /proc twice.
    let mem_mb = system.mem.map(|m| m.total_mb).unwrap_or(0);

    let cpu_count = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    system.cpu_count = cpu_count;

    // `hostname -I` lists every address; the first is the primary one. Falling back to
    // the wildcard is better than showing nothing, and the footer treats "*" as unknown.
    let ip = run_cmd("hostname", &["-I"])
        .and_then(|o| o.split_whitespace().next().map(|s| s.to_string()))
        .unwrap_or_default();

    let gpus = run_cmd(
        "nvidia-smi",
        &[
            "--query-gpu=name,memory.total,memory.used,temperature.gpu,fan.speed,utilization.gpu",
            "--format=csv,noheader,nounits",
        ],
    )
    .map(|o| parse_nvidia_smi(&o))
    .unwrap_or_default();

    let disks = run_cmd("lsblk", &["-bdno", "NAME,SIZE,TYPE"])
        .map(|o| parse_lsblk(&o))
        .unwrap_or_default();

    let net = collect_net();

    Machine { hostname, ip, distro, kernel, mem_mb, cpu_count, gpus, disks, net, system }
}

/// `PRETTY_NAME="Linux Mint 22.3"` -> `Linux Mint 22.3`
pub(crate) fn parse_pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        line.strip_prefix("PRETTY_NAME=")
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    })
}

/// The distribution of the machine being documented.
///
/// Inside a container `/etc/os-release` describes the *image*, which would put
/// "Debian GNU/Linux 12 (bookworm)" under a dashboard about a Mint host. The compose
/// file mounts the host's file at `/host/etc/os-release`; `SPLASH_OS_RELEASE` overrides
/// the location.
pub(crate) fn collect_distro() -> String {
    let candidates = [
        std::env::var("SPLASH_OS_RELEASE").ok(),
        Some("/host/etc/os-release".to_string()),
        Some("/etc/os-release".to_string()),
    ];
    for path in candidates.into_iter().flatten() {
        if let Some(name) = std::fs::read_to_string(&path).ok().as_deref().and_then(parse_pretty_name) {
            return name;
        }
    }
    String::new()
}

/// Does a systemd unit describe this process?
///
/// Deliberately strict. A loose `process.contains(unit)` test made `docker-proxy`
/// match `docker.service`, which pulled that unit's description — "Docker Application
/// Container Engine" — into the evidence for every published container port. The
/// result was Dockge, nginx and four other services all being renamed "Docker".
///
/// The `d` suffix covers the common daemon naming (`sshd` for `ssh.service`, `mysqld`
/// for `mysql.service`, `smbd` for `smb.service`) without matching unrelated units.
pub(crate) fn unit_matches_process(unit: &str, process: &str) -> bool {
    let base = unit.trim_end_matches(".service").to_lowercase();
    let needle = process.to_lowercase();
    base == needle || needle == format!("{base}d") || base == format!("{needle}d")
}

/// Listening TCP sockets, preferring `ss -p` and degrading to `/proc/net/tcp`.
pub(crate) fn collect_listeners() -> Vec<(u16, bool, Option<String>, Option<u32>)> {
    if let Some(out) = run_cmd("ss", &["-ltnp"]) {
        let parsed = parse_ss(&out);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Some(out) = run_cmd("ss", &["-ltn"]) {
        let parsed = parse_ss(&out);
        if !parsed.is_empty() {
            return parsed;
        }
    }
    let mut out = std::fs::read_to_string("/proc/net/tcp")
        .map(|c| parse_proc_net_tcp(&c))
        .unwrap_or_default();
    if let Ok(v6) = std::fs::read_to_string("/proc/net/tcp6") {
        out.extend(parse_proc_net_tcp(&v6));
    }
    out
}

/// Run a full discovery pass.
pub(crate) fn run(opts: &Options) -> Discovery {
    let machine = collect_machine();

    // ── listeners, deduped by port (v4 and v6 usually both bind the same port) ──
    let mut by_port: BTreeMap<u16, DiscoveredService> = BTreeMap::new();
    for (port, remote_reachable, proc_name, pid) in collect_listeners() {
        if opts.exclude_ports.contains(&port) || !opts.keep_port(port) {
            continue;
        }
        let entry = by_port.entry(port).or_insert_with(|| DiscoveredService {
            port,
            remote_reachable,
            proc_name: None,
            pid: None,
            unit: None,
            unit_desc: None,
            container: None,
            image: None,
            http: None,
        });
        // A routable bind wins over a loopback bind of the same port.
        entry.remote_reachable |= remote_reachable;
        if entry.proc_name.is_none() {
            entry.proc_name = proc_name;
        }
        if entry.pid.is_none() {
            entry.pid = pid;
        }
    }

    // ── systemd units ──
    let mut units: Vec<(String, String)> = Vec::new();
    for args in [
        vec!["list-units", "--type=service", "--state=running", "--no-legend", "--plain", "--no-pager"],
        vec!["--user", "list-units", "--type=service", "--state=running", "--no-legend", "--plain", "--no-pager"],
    ] {
        if let Some(out) = run_cmd("systemctl", &args) {
            units.extend(parse_systemd_units(&out));
        }
    }

    // ── docker containers ──
    let containers = run_cmd("docker", &["ps", "--format", "{{json .}}"])
        .map(|o| parse_docker_json(&o))
        .unwrap_or_default();

    for (names, image, host_ports) in &containers {
        for port in host_ports {
            if opts.exclude_ports.contains(port) || !opts.keep_port(*port) {
                continue;
            }
            let entry = by_port.entry(*port).or_insert_with(|| DiscoveredService {
                port: *port,
                remote_reachable: true,
                proc_name: None,
                pid: None,
                unit: None,
                unit_desc: None,
                container: None,
                image: None,
                http: None,
            });
            if entry.container.is_none() {
                entry.container = Some(names.clone());
                entry.image = Some(image.clone());
            }
        }
    }

    // ── attach systemd descriptions to processes that match a unit name ──
    for entry in by_port.values_mut() {
        let Some(proc_name) = entry.proc_name.clone() else { continue };
        // A port forwarder says nothing about which unit is behind it.
        if is_runtime_plumbing(&proc_name) {
            continue;
        }
        if let Some((unit, desc)) = units.iter().find(|(u, _)| unit_matches_process(u, &proc_name)) {
            entry.unit = Some(unit.clone());
            if !desc.is_empty() {
                entry.unit_desc = Some(desc.clone());
            }
        }
    }

    let mut services: Vec<DiscoveredService> = by_port
        .into_values()
        .filter(|s| opts.include_loopback || s.remote_reachable)
        .collect();

    // ── classify each port over HTTP ──
    if opts.probe_http {
        for svc in services.iter_mut() {
            svc.http = probe_http(svc.port, opts.http_timeout);
        }
    }

    Discovery { machine, services }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SS_SAMPLE: &str = "\
State  Recv-Q Send-Q Local Address:Port  Peer Address:Port Process
LISTEN 0      4096         0.0.0.0:80           0.0.0.0:*    users:((\"apache2\",pid=1234,fd=4))
LISTEN 0      128             [::]:22              [::]:*    users:((\"sshd\",pid=900,fd=3))
LISTEN 0      4096       127.0.0.1:631          0.0.0.0:*    users:((\"cupsd\",pid=1000,fd=7))
LISTEN 0      511          0.0.0.0:11434        0.0.0.0:*
LISTEN 0      70                 *:8080               *:*    users:((\"docker-proxy\",pid=42,fd=9))
";

    #[test]
    fn parses_ss_listen_sockets() {
        let parsed = parse_ss(SS_SAMPLE);
        assert_eq!(parsed.len(), 5, "every LISTEN line should yield one entry: {parsed:?}");

        let http = parsed.iter().find(|(p, ..)| *p == 80).unwrap();
        assert!(http.1, "0.0.0.0:80 is remotely reachable");
        assert_eq!(http.2.as_deref(), Some("apache2"));
        assert_eq!(http.3, Some(1234));

        let v6 = parsed.iter().find(|(p, ..)| *p == 22).unwrap();
        assert!(v6.1, "[::]:22 is remotely reachable");
        assert_eq!(v6.2.as_deref(), Some("sshd"));

        let cups = parsed.iter().find(|(p, ..)| *p == 631).unwrap();
        assert!(!cups.1, "127.0.0.1:631 is loopback only");

        let no_proc = parsed.iter().find(|(p, ..)| *p == 11434).unwrap();
        assert_eq!(no_proc.2, None, "ss -ltn has no process column");

        let star = parsed.iter().find(|(p, ..)| *p == 8080).unwrap();
        assert!(star.1);
        assert_eq!(star.2.as_deref(), Some("docker-proxy"));
    }

    #[test]
    fn ignores_non_listen_lines() {
        let sample = "State  Recv-Q Send-Q Local Address:Port\nESTAB 0 0 10.0.0.1:1234 10.0.0.2:80\n";
        assert!(parse_ss(sample).is_empty());
    }

    #[test]
    fn parses_docker_json_and_published_ports() {
        let sample = r#"{"Names":"zigbee2mqtt","Image":"koenkk/zigbee2mqtt","Ports":"0.0.0.0:8080->8080/tcp, [::]:8080->8080/tcp"}
{"Names":"ollama","Image":"ollama/ollama","Ports":"127.0.0.1:11434->11434/tcp"}
{"Names":"internal","Image":"redis","Ports":"6379/tcp"}"#;
        let parsed = parse_docker_json(sample);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "zigbee2mqtt");
        assert_eq!(parsed[0].2, vec![8080]);
        assert_eq!(parsed[1].2, vec![11434]);
        assert!(parsed[2].2.is_empty(), "exposed-but-unpublished ports are not reachable");
    }

    #[test]
    fn dedupes_repeated_published_ports() {
        assert_eq!(parse_published_ports("0.0.0.0:80->80/tcp, [::]:80->80/tcp"), vec![80]);
    }

    #[test]
    fn parses_systemd_units_with_and_without_bullet() {
        let sample = "\
  apache2.service     loaded active running The Apache HTTP Server
● ollama.service      loaded active running Ollama LLM server
not-a-unit line here
";
        let parsed = parse_systemd_units(sample);
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        assert_eq!(parsed[0].0, "apache2.service");
        assert_eq!(parsed[0].1, "The Apache HTTP Server");
        assert_eq!(parsed[1].0, "ollama.service");
        assert_eq!(parsed[1].1, "Ollama LLM server");
    }

    #[test]
    fn parses_nvidia_smi_csv() {
        let sample = "NVIDIA GeForce RTX 3090, 24576, 16070, 39, 47, 0\nNVIDIA GeForce RTX 3090, 24576, 0, 29, 0, 12\nAMD GPU, 0, 0, 37, N/A, N/A";
        let gpus = parse_nvidia_smi(sample);
        assert_eq!(gpus.len(), 3);
        assert_eq!(gpus[0].mem_pct, 65, "16070/24576 = 65%");
        assert_eq!(gpus[0].temp_c, Some(39));
        assert_eq!(gpus[0].fan_pct, Some(47));
        assert_eq!(gpus[1].util_pct, Some(12));
        assert_eq!(gpus[2].mem_pct, 0, "zero total must not divide by zero");
        assert_eq!(gpus[2].fan_pct, None, "N/A is not a number");
    }

    #[test]
    fn parses_memory_usage_as_used_of_total() {
        let sample = "\
MemTotal:       16384000 kB
MemFree:         2000000 kB
MemAvailable:    8192000 kB
Buffers:          100000 kB
Cached:          3000000 kB
SwapTotal:       8192000 kB
SwapFree:        6144000 kB
";
        let mem = parse_meminfo(sample).expect("meminfo parses");
        assert_eq!(mem.total_mb, 16000);
        assert_eq!(mem.available_mb, 8000);
        assert_eq!(mem.used_mb, 8000, "used is total minus what a workload could claim");
        assert_eq!(mem.swap_total_mb, 8000);
        assert_eq!(mem.swap_used_mb, 2000);

        let stats = SystemStats { mem: Some(mem), ..SystemStats::default() };
        assert_eq!(stats.mem_pct(), Some(50));
    }

    #[test]
    fn memory_falls_back_when_mem_available_is_absent() {
        // Kernels before 3.14: free + buffers + cached is the old definition of
        // "available", and without the fallback the card would claim all RAM is used.
        let sample = "\
MemTotal:        8192000 kB
MemFree:         1024000 kB
Buffers:          102400 kB
Cached:          2048000 kB
";
        let mem = parse_meminfo(sample).unwrap();
        assert_eq!(mem.total_mb, 8000);
        assert_eq!(mem.available_mb, 3100);
        assert_eq!(mem.used_mb, 4900);
        assert_eq!(mem.swap_total_mb, 0);

        assert!(parse_meminfo("").is_none(), "a file with no MemTotal is not usable");
    }

    #[test]
    fn parses_load_averages() {
        let load = parse_loadavg("2.83 2.66 2.46 2/3621 334586").expect("loadavg parses");
        assert_eq!(load.one, 2.83);
        assert_eq!(load.five, 2.66);
        assert_eq!(load.fifteen, 2.46);

        assert!(parse_loadavg("").is_none());
        assert!(parse_loadavg("not a load average").is_none());
        assert!(parse_loadavg("1.00 0.50").is_none(), "all three windows are required");
    }

    #[test]
    fn cpu_usage_is_a_delta_not_an_average_since_boot() {
        let first = parse_cpu_times(
            "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8\n",
        )
        .expect("aggregate cpu line parses");

        // 300 new ticks, of which 100 were idle: 200/300 busy.
        let second = parse_cpu_times("cpu  200 0 200 900 0 0 0 0 0 0\n").unwrap();
        assert_eq!(cpu_usage_pct(&first, &second), Some(67));

        // No busy ticks at all.
        let idle_only = parse_cpu_times("cpu  100 0 100 900 0 0 0 0 0 0\n").unwrap();
        assert_eq!(cpu_usage_pct(&first, &idle_only), Some(0));

        // Two reads at the same instant cannot produce a percentage.
        assert_eq!(cpu_usage_pct(&first, &first), None);

        assert!(parse_cpu_times("intr 1 2 3\n").is_none());
        assert!(parse_cpu_times("cpu  x 0 0 0\n").is_none(), "non-numeric counters are rejected");
    }

    #[test]
    fn summarises_identical_gpus() {
        let mk = |n: &str| Gpu { name: n.to_string(), mem_pct: 0, temp_c: None, fan_pct: None, util_pct: None };
        let m = Machine {
            gpus: vec![mk("NVIDIA GeForce RTX 3090"), mk("NVIDIA GeForce RTX 3090"), mk("AMD GPU")],
            distro: "Linux Mint 22.3".into(),
            ..Machine::default()
        };
        assert_eq!(m.gpu_summary().as_deref(), Some("Dual RTX 3090 + AMD GPU"));
        assert_eq!(m.subtitle(), "Dual RTX 3090 + AMD GPU — Linux Mint 22.3 — services directory");
    }

    #[test]
    fn extracts_page_title() {
        assert_eq!(extract_title("<html><head><TITLE>\n  Grafana  </TITLE></head>").as_deref(), Some("Grafana"));
        assert_eq!(extract_title("<html><body>no title").as_deref(), None);
        assert_eq!(extract_title("<title>   </title>").as_deref(), None);
    }

    #[test]
    fn ephemeral_ports_are_filtered_unless_asked_for() {
        // Pin the standard Linux range so the test does not depend on this machine.
        let default = Options { ephemeral: (32768, 60999), ..Options::default() };

        assert!(default.keep_port(22));
        assert!(default.keep_port(11434));
        assert!(default.keep_port(32767), "just below the range");
        assert!(!default.keep_port(32768), "the range starts here");
        assert!(!default.keep_port(35404), "test-container port from docker ps");
        assert!(!default.keep_port(60999), "and ends here, inclusively");

        // Real regression: Glances listens on 61208, just above the range. Excluding
        // "everything above 32767" hid it from the page entirely.
        assert!(default.keep_port(61208), "Glances must not be filtered out");
        assert!(default.keep_port(61000));

        let inclusive = Options { include_ephemeral: true, ..Options::default() };
        assert!(inclusive.keep_port(35404));
    }

    #[test]
    fn parses_the_kernel_ephemeral_range() {
        assert_eq!(parse_ephemeral_range("32768 60999"), Some((32768, 60999)));
        assert_eq!(parse_ephemeral_range("32768\t60999"), Some((32768, 60999)), "tabs too");
        assert_eq!(parse_ephemeral_range(" 1024 65535 "), Some((1024, 65535)));
        assert_eq!(parse_ephemeral_range("60999 32768"), None, "inverted is not usable");
        assert_eq!(parse_ephemeral_range("nonsense"), None);
        assert_eq!(parse_ephemeral_range(""), None);

        let (low, high) = ephemeral_range();
        assert!(low < high, "a real range, whatever this machine reports");
    }

    #[test]
    fn humanises_sizes_and_link_rates() {
        assert_eq!(human_bytes(64 * 1024 * 1024 * 1024), "64 GB");
        assert_eq!(human_bytes(4_000_787_030_016), "3.6 TB", "binary, as system tools report RAM");
        // Storage uses decimal so a "4 TB" disk reads as 4 TB, the way it is sold.
        assert_eq!(human_storage(4_000_787_030_016), "4.0 TB");
        assert_eq!(human_storage(3 * 4_000_787_030_016), "12 TB");
        assert_eq!(human_storage(500_107_862_016), "500 GB");
        assert_eq!(human_bytes(4 * 1024 * 1024 * 1024 * 1024), "4.0 TB");
        assert_eq!(human_bytes(512 * 1024 * 1024), "512 MB");
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bitrate(1000), "1 Gb/s");
        assert_eq!(human_bitrate(100), "100 Mb/s");
        assert_eq!(human_bitrate(10000), "10 Gb/s");
        assert_eq!(human_bitrate(2500), "2.5 Gb/s");
    }

    #[test]
    fn parses_lsblk_disks_only() {
        let sample = "\
sda 4000787030016 disk
├─sda1 536870912 part
└─sda2 3999358918656 part
sdb 1000204886016 disk
loop0 123456 loop
sr0 1073741824 rom
";
        let disks = parse_lsblk(sample);
        assert_eq!(disks.len(), 2, "only real disks count: {disks:?}");
        assert_eq!(disks[0].name, "sda");
        assert_eq!(disks[1].size_bytes, 1_000_204_886_016);
    }

    #[test]
    fn builds_the_stat_line_from_what_is_known() {
        let m = Machine {
            mem_mb: 64 * 1024,
            cpu_count: 24,
            disks: vec![
                Disk { name: "nvme0n1".into(), size_bytes: 2_000_398_934_016 },
                Disk { name: "nvme1n1".into(), size_bytes: 2_000_398_934_016 },
            ],
            net: vec![
                NetIface { name: "eno1".into(), speed_mbps: 1000, state: "up".into(), physical: true },
                NetIface { name: "docker0".into(), speed_mbps: -1, state: "down".into(), physical: false },
            ],
            ..Machine::default()
        };
        let chips = m.stat_chips();
        assert_eq!(chips, vec!["64 GB RAM", "4.0 TB across 2 disks", "24 cores", "eno1 1 Gb/s"]);
        assert!(!chips.iter().any(|c| c.contains("docker0")), "virtual interfaces are skipped");
        assert_eq!(Machine::default().stat_chips(), Vec::<String>::new());
    }

    /// Observed on the real host: Docker bridges report 10 Gb/s, which outranked and
    /// hid the physical NIC the machine is actually reached over.
    #[test]
    fn docker_bridges_do_not_mask_the_physical_nic() {
        let m = Machine {
            net: vec![
                NetIface { name: "br-0076f5b84c61".into(), speed_mbps: 10000, state: "up".into(), physical: false },
                NetIface { name: "br-443c1e599d15".into(), speed_mbps: 10000, state: "up".into(), physical: false },
                NetIface { name: "eno1".into(), speed_mbps: 1000, state: "up".into(), physical: true },
            ],
            ..Machine::default()
        };
        let chips = m.stat_chips();
        assert_eq!(chips, vec!["eno1 1 Gb/s"], "only the real device is shown");
        assert!(!chips.iter().any(|c| c.contains("br-")), "{chips:?}");

        // With no physical device at all, a usable rate still beats showing nothing.
        let only_virtual = Machine {
            net: vec![NetIface { name: "tun0".into(), speed_mbps: 100, state: "up".into(), physical: false }],
            ..Machine::default()
        };
        assert_eq!(only_virtual.stat_chips(), vec!["tun0 100 Mb/s"]);
    }

    #[test]
    fn reads_the_pretty_name_from_a_release_file() {
        let mint = "NAME=\"Linux Mint\"\nVERSION=\"22.3\"\nPRETTY_NAME=\"Linux Mint 22.3\"\nID=linuxmint\n";
        assert_eq!(parse_pretty_name(mint).as_deref(), Some("Linux Mint 22.3"));
        assert_eq!(parse_pretty_name("ID=debian\n").as_deref(), None);
        assert_eq!(parse_pretty_name("PRETTY_NAME=\"\"\n").as_deref(), None);
    }

    #[test]
    fn spots_container_ids_masquerading_as_hostnames() {
        assert!(looks_like_container_id("f3a9c1b2d4e5"));
        assert!(looks_like_container_id("0123456789ab"));
        assert!(!looks_like_container_id("ginger"));
        assert!(!looks_like_container_id("ginger.local"));
        assert!(!looks_like_container_id("dsh-ctl-deepseek-harness-1"));
        assert!(!looks_like_container_id("abcdefabcdef1"), "13 chars");
        assert!(!looks_like_container_id("abcdefabcdefg"), "not hex");
    }

    #[test]
    fn units_match_processes_strictly() {
        // The daemon naming that should work.
        assert!(unit_matches_process("apache2.service", "apache2"));
        assert!(unit_matches_process("ssh.service", "sshd"), "sshd runs ssh.service");
        assert!(unit_matches_process("mysql.service", "mysqld"));
        assert!(unit_matches_process("smb.service", "smbd"));
        assert!(unit_matches_process("docker.service", "dockerd"));
        assert!(unit_matches_process("mosquitto.service", "mosquitto"));

        // The regression: this attached Docker's own description to every port Docker
        // forwards, renaming Dockge and nginx to "Docker".
        assert!(!unit_matches_process("docker.service", "docker-proxy"));
        assert!(!unit_matches_process("node-exporter.service", "node"));
        assert!(!unit_matches_process("containerd.service", "containerd-shim"));
        assert!(!unit_matches_process("ssh.service", "ssh-agent"));
    }

    #[test]
    fn runtime_plumbing_is_not_an_identity() {
        let mut dockge = DiscoveredService {
            port: 5001,
            remote_reachable: true,
            proc_name: Some("docker-proxy".into()),
            pid: Some(1),
            unit: None,
            unit_desc: None,
            container: Some("dockge".into()),
            image: Some("louislam/dockge:1".into()),
            http: None,
        };
        assert_eq!(dockge.identity(), "dockge", "the container names the service");

        // With no container name to fall back on, docker-proxy is still not the answer.
        dockge.container = None;
        dockge.image = None;
        assert_eq!(dockge.identity(), "port-5001");
        assert_eq!(dockge.id(), "port:5001");

        // Ordinary processes are still perfectly good identities.
        let sshd = DiscoveredService { proc_name: Some("sshd".into()), port: 22, ..dockge.clone() };
        assert_eq!(sshd.identity(), "sshd");
        assert!(is_runtime_plumbing("Docker-Proxy"), "case-insensitive");
        assert!(is_runtime_plumbing("containerd-shim"));
        assert!(!is_runtime_plumbing("dockerd"));
    }

    #[test]
    fn qualifies_bare_hostnames_only() {
        assert_eq!(qualify_host("ginger", ".local"), "ginger.local");
        assert_eq!(qualify_host("ginger.local", ".local"), "ginger.local");
        assert_eq!(qualify_host("ginger.phaseshift.studio", ".local"), "ginger.phaseshift.studio");
        assert_eq!(qualify_host("192.168.1.72", ".local"), "192.168.1.72");
        assert_eq!(qualify_host("127.0.0.1", ".local"), "127.0.0.1");
        assert_eq!(qualify_host("::1", ".local"), "::1");
        assert_eq!(qualify_host("localhost", ".local"), "localhost");
        assert_eq!(qualify_host("", ".local"), "");
        assert_eq!(qualify_host("ginger", ""), "ginger", "an empty suffix disables this");
        assert_eq!(qualify_host("ginger", "lan"), "ginger.lan", "a suffix without a dot still works");
    }

    #[test]
    fn spots_urls_the_browser_would_read_as_its_own_machine() {
        assert!(is_viewer_localhost("http://localhost:61208"));
        assert!(is_viewer_localhost("http://127.0.0.1:61208"));
        assert!(is_viewer_localhost("http://127.0.0.1:61208/api/4/gpu"));
        assert!(!is_viewer_localhost("http://ginger.local:61208"));
        assert!(!is_viewer_localhost("http://192.168.1.72:61208"));
    }

    #[test]
    fn finds_declared_icon_links() {
        assert_eq!(
            extract_icon_href(r#"<link rel="icon" href="/favicon.png"/>"#).as_deref(),
            Some("/favicon.png")
        );
        assert_eq!(
            extract_icon_href(r#"<link rel="shortcut icon" href="static/icons/fav.ico">"#).as_deref(),
            Some("static/icons/fav.ico")
        );
        assert_eq!(
            extract_icon_href(r#"<LINK REL='ICON' HREF='/a/b.png'>"#).as_deref(),
            Some("/a/b.png")
        );
        assert_eq!(extract_icon_href("<html><head><title>x</title></head>"), None);
        assert_eq!(extract_icon_href(r#"<link rel="stylesheet" href="/s.css">"#), None);
    }

    #[test]
    fn resolves_icon_hrefs_against_the_service() {
        let base = "http://127.0.0.1:8080";
        assert_eq!(resolve_url(base, "/favicon.ico"), "http://127.0.0.1:8080/favicon.ico");
        assert_eq!(resolve_url(base, "img/i.png"), "http://127.0.0.1:8080/img/i.png");
        assert_eq!(resolve_url(base, "//cdn.example/i.png"), "http://cdn.example/i.png");
        assert_eq!(resolve_url(base, "https://x/i.png"), "https://x/i.png");
    }

    #[test]
    fn sniffs_images_by_content_not_by_url() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A];
        let ico = [0x00, 0x00, 0x01, 0x00, 0x01];
        assert_eq!(sniff_image_ext(&png, "").as_deref(), Some("png"));
        assert_eq!(sniff_image_ext(&ico, "image/vnd.microsoft.icon").as_deref(), Some("ico"));
        assert_eq!(sniff_image_ext(&[0xFF, 0xD8, 0xFF, 0xE0], "").as_deref(), Some("jpg"));
        assert_eq!(sniff_image_ext(b"GIF89a...", "").as_deref(), Some("gif"));
        assert_eq!(sniff_image_ext(b"<svg xmlns=\"...\">", "image/svg+xml").as_deref(), Some("svg"));
        // A 200 HTML error page pretending to be an icon must be rejected.
        assert_eq!(sniff_image_ext(b"<!DOCTYPE html><html>not found", "image/x-icon"), None);
        assert_eq!(sniff_image_ext(b"", "image/png"), None);
    }

    #[test]
    fn parses_proc_net_tcp_listen_sockets() {
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0277 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0\n\
   1: 00000000:0016 00000000:0000 01 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0\n";
        let parsed = parse_proc_net_tcp(sample);
        assert_eq!(parsed.len(), 1, "only state 0A (LISTEN) counts");
        assert_eq!(parsed[0].0, 631);
        assert!(!parsed[0].1, "0100007F is 127.0.0.1");
    }
}

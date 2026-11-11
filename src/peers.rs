//! Other machines running server-splash, so the dashboard can link to them.
//!
//! The page for one host answers "what is running *here*". This module answers the
//! next question — "and how do I get to the other boxes?" — from a hand-written
//! `peers.toml`. It is deliberately a separate file from `splash-config.toml`: the
//! wizard rewrites the config when you change a setting, and a peer list is a thing
//! you keep editing by hand (and, later, a thing discovery appends to), so keeping
//! them apart means neither can clobber the other.
//!
//! Everything here is a pure function over text so the tests need no network, in
//! keeping with the rest of the tool.
//!
//! ```toml
//! [[peer]]
//! name = "ginger"
//! url  = "ginger.local:6420"
//! desc = "Dual RTX 3090 — media + GPUs"
//! group = "LAN"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Icon used when a peer does not name one.
pub(crate) const DEFAULT_PEER_ICON: &str = "🖥️";

/// Where a peer entry came from. Only `config` entries are ever republished: a
/// machine we merely heard about must not be re-advertised as if we vouched for it,
/// which is also what bounds gossip to a single hop.
pub(crate) const SOURCE_CONFIG: &str = "config";
pub(crate) const SOURCE_MDNS: &str = "mdns";
pub(crate) const SOURCE_FEDERATION: &str = "federation";
/// A machine remembered from an earlier run. A hint, not a fact: see `cache.rs`.
pub(crate) const SOURCE_CACHE: &str = "cache";

/// One other machine whose splash page should be linked from this one.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub(crate) struct Peer {
    /// Display name, e.g. `ginger`. Required — a nameless peer is skipped.
    #[serde(default)]
    pub name: String,
    /// Where that machine's splash page lives. A bare name gets a scheme and the
    /// configured host suffix, so `ginger` and `ginger:6420` both work.
    #[serde(default)]
    pub url: String,
    /// One line under the name. Defaults to the URL's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    /// Emoji, an image path, or an absolute URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional tag shown on the card, e.g. `LAN` or `WAN`. Purely presentational.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Keep the peer in the file but off the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// Status-light the peer from the browser. On by default; turn it off for a
    /// machine behind a slow link where a 4-second probe is just noise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<bool>,
    /// Publish this entry in `peers.json` for other dashboards to learn from. On by
    /// default; a hidden peer is never published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertise: Option<bool>,
    /// Where this entry came from: `config`, `mdns` or `federation`. Deliberately not
    /// serialised — it describes the run, not the file.
    #[serde(skip)]
    pub source: Option<String>,
    /// The dashboard that told us about this machine, when it arrived over federation.
    /// Shown on the card so a learned address is never mistaken for a curated one.
    #[serde(skip)]
    pub learned_from: Option<String>,
    /// Addresses this machine was resolved to, when it came from the network. Used to
    /// tell that `192.168.1.50:6420` in the file and `ginger.local:6420` on the wire are
    /// the same box, so the page does not offer two cards for one machine.
    #[serde(skip)]
    pub addrs: Vec<String>,
    /// When this machine was last seen, as epoch seconds. Only set on an entry read back
    /// from the on-disk cache, where it is the whole point: an address remembered from
    /// last week is a hint to try, and the page says how old the hint is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// Whether the generation-time fetch reached this machine. `None` means nobody asked
    /// (it is not a configured or remembered address), and the browser decides instead.
    #[serde(skip)]
    pub alive: Option<bool>,
}

impl Peer {
    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden.unwrap_or(false)
    }

    /// Whether the card should carry a live status light.
    pub(crate) fn probes(&self) -> bool {
        self.probe.unwrap_or(true)
    }

    /// Whether this entry may be republished in `peers.json`.
    ///
    /// A hidden peer is never published: "hidden" is a statement about visibility, and
    /// a list we serve to strangers is the most visible place a peer could go.
    pub(crate) fn advertises(&self) -> bool {
        !self.is_hidden() && self.advertise.unwrap_or(true)
    }

    pub(crate) fn icon_or_default(&self) -> String {
        self.icon
            .clone()
            .filter(|i| !i.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PEER_ICON.to_string())
    }

    /// What to print on the right-hand chip: the URL without its scheme.
    ///
    /// `http://ginger.local:6420` reads better as `ginger.local:6420`; the scheme is
    /// already obvious from the fact that clicking it works.
    pub(crate) fn display_target(&self) -> String {
        self.url
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string()
    }

    /// The description to render, falling back to the URL's host.
    pub(crate) fn desc_or_default(&self) -> String {
        match self.desc.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(d) => d.to_string(),
            None => self.display_target(),
        }
    }
}

/// The file shape: `[[peer]]` tables.
#[derive(Deserialize, Serialize, Debug, Default)]
pub(crate) struct PeerFile {
    #[serde(default)]
    pub peer: Vec<Peer>,
}

/// Turn whatever was written for a peer's address into a URL a browser can follow.
///
/// Accepts `ginger`, `ginger:6420`, `ginger.local/splash`, `http://…` and `https://…`.
/// A bare hostname is qualified with `host_suffix` (`.local` by default) so it
/// resolves when a browser clicks it, exactly as service links are. Anything that is
/// not plainly http(s) is refused rather than guessed at: a stray `ftp://` in a peer
/// list should not become a link on the dashboard.
pub(crate) fn normalize_url(raw: &str, host_suffix: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    let (scheme, rest) = match raw.split_once("://") {
        Some((scheme, rest)) => {
            let scheme = scheme.trim().to_ascii_lowercase();
            if scheme != "http" && scheme != "https" {
                return None;
            }
            (scheme, rest)
        }
        // No scheme, so this is a host (optionally with a port and path).
        None => ("http".to_string(), raw),
    };

    let rest = rest.trim_start_matches("//").trim();
    if rest.is_empty() {
        return None;
    }

    // Split the authority from the path *before* splitting host from port.
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return None;
    }

    let (host, port) = split_host_port(authority);
    if host.is_empty() {
        return None;
    }
    let host = crate::discover::qualify_host(host, host_suffix);
    let path = path.trim_end_matches('/');

    Some(match port {
        Some(port) => format!("{scheme}://{host}:{port}{path}"),
        None => format!("{scheme}://{host}{path}"),
    })
}

/// `ginger.local:6420` -> (`ginger.local`, Some("6420")); `[::1]:6420` survives.
fn split_host_port(authority: &str) -> (&str, Option<&str>) {
    // An IPv6 literal is `[...]`, and its colons are not a port separator.
    let (head, tail) = match authority.rfind(']') {
        Some(end) => authority.split_at(end + 1),
        None => match authority.rsplit_once(':') {
            Some((head, _)) => authority.split_at(head.len()),
            None => (authority, ""),
        },
    };
    match tail.strip_prefix(':') {
        Some(port) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (head, Some(port)),
        _ => (authority, None),
    }
}

/// Parse a `peers.toml` body into normalized, de-duplicated peers.
///
/// A malformed peer is reported and skipped rather than failing the whole page: a typo
/// in a machine nobody is currently using should not stop the dashboard generating.
pub(crate) fn parse(text: &str, host_suffix: &str) -> Result<Vec<Peer>, String> {
    let file: PeerFile = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(normalize(file.peer, host_suffix))
}

/// Normalize one entry and stamp where it came from.
///
/// Shared by the three sources — the config file, mDNS and federation — so a peer
/// looks the same whichever way it arrived.
pub(crate) fn normalize_one(mut peer: Peer, host_suffix: &str, source: &str) -> Option<Peer> {
    peer.name = peer.name.trim().to_string();
    if peer.name.is_empty() {
        return None;
    }
    peer.url = normalize_url(&peer.url, host_suffix)?;
    peer.source = Some(source.to_string());
    Some(peer)
}

/// Normalize a list of peers: fill in defaults, drop unusable entries, de-dupe.
fn normalize(peers: Vec<Peer>, host_suffix: &str) -> Vec<Peer> {
    let mut out: Vec<Peer> = Vec::with_capacity(peers.len());
    let mut seen: Vec<String> = Vec::new();

    for peer in peers {
        let name = peer.name.trim().to_string();
        if name.is_empty() {
            eprintln!("  \x1b[33mwarning\x1b[0m: a [[peer]] has no name; skipping it");
            continue;
        }
        let raw_url = peer.url.clone();
        let Some(peer) = normalize_one(peer, host_suffix, SOURCE_CONFIG) else {
            eprintln!(
                "  \x1b[33mwarning\x1b[0m: peer {name:?} has an unusable url {raw_url:?} (want http(s), or host[:port]); skipping it"
            );
            continue;
        };
        let key = peer_key(&peer.url);
        if seen.contains(&key) {
            eprintln!(
                "  \x1b[33mwarning\x1b[0m: peer {name:?} repeats {}; the first entry wins",
                peer.url
            );
            continue;
        }
        seen.push(key);
        out.push(peer);
    }
    out
}

/// Read a peers file. A missing file is not an error — it just means "no peers yet".
pub(crate) fn load(path: &Path, host_suffix: &str) -> Vec<Peer> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!(
                "  \x1b[33mwarning\x1b[0m: cannot read {} ({e}); no peer machines will be linked",
                path.display()
            );
            return Vec::new();
        }
    };

    match parse(&text, host_suffix) {
        Ok(peers) => peers,
        Err(e) => {
            eprintln!(
                "  \x1b[33mwarning\x1b[0m: {} is not valid TOML ({e}); ignoring it",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Identity of a machine, independent of how its URL was spelled.
///
/// `http://ginger.local:6420/` and `https://GINGER.local:6420` are the same box, so
/// discovery can tell that a machine it just heard about over mDNS is one already
/// listed by hand. The key is the authority, lowercase: `ginger.local:6420`.
pub(crate) fn peer_key(url: &str) -> String {
    let rest = url.trim();
    let rest = rest
        .strip_prefix("https://")
        .or_else(|| rest.strip_prefix("http://"))
        .unwrap_or(rest);
    let authority = rest.split('/').next().unwrap_or("").trim_end_matches('/');
    authority.to_ascii_lowercase()
}

/// Split a peer key into its host and port halves.
fn split_key(key: &str) -> (&str, Option<u16>) {
    if key.starts_with('[') {
        // An IPv6 literal: the port, if any, is after the closing bracket.
        return match key.rfind(']') {
            Some(end) => match key[end + 1..].strip_prefix(':') {
                Some(port) => (key, port.parse().ok()),
                None => (key, None),
            },
            None => (key, None),
        };
    }
    match key.rsplit_once(':') {
        Some((host, port)) => match port.parse::<u16>() {
            Ok(port) => (host, Some(port)),
            Err(_) => (key, None),
        },
        None => (key, None),
    }
}

/// Strip the `.local` suffix so `ginger` and `ginger.local` compare equal.
fn bare_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    host.strip_suffix(".local").unwrap_or(&host).to_string()
}

/// Is this URL this very machine?
///
/// Without this the page grows a card linking to itself: our own announcement comes
/// back through the same mDNS browse that discovers everyone else.
pub(crate) fn is_self(url: &str, hostname: &str, port: Option<u16>) -> bool {
    let self_host = bare_host(hostname);
    if self_host.is_empty() {
        return false;
    }
    let key = peer_key(url);
    let (peer_host, peer_port) = split_key(&key);
    if bare_host(peer_host) != self_host {
        return false;
    }
    match port {
        Some(port) => peer_port == Some(port),
        // Not serving, so we cannot say: any peer on our own hostname is us.
        None => true,
    }
}

/// Append peers we did not already know about, keeping the existing entry for a
/// machine that is listed twice. The first source wins, so a hand-written entry is
/// never replaced by a discovered one.
/// The identities one machine answers to: its hostname, plus every address it resolved
/// to. A peer written down as `192.168.1.50:6420` and the same peer announcing itself as
/// `ginger.local:6420` are one machine, and the page should say so once.
fn identities(peer: &Peer) -> Vec<String> {
    let key = peer_key(&peer.url);
    let (host, port) = split_key(&key);
    let suffix = match port {
        Some(port) => format!(":{port}"),
        None => String::new(),
    };
    let mut ids = vec![format!("{host}{suffix}")];
    for addr in &peer.addrs {
        ids.push(format!("{}{suffix}", addr.trim().to_ascii_lowercase()));
    }
    ids
}

pub(crate) fn merge(base: Vec<Peer>, extra: impl IntoIterator<Item = Peer>) -> Vec<Peer> {
    let mut seen: HashSet<String> = base.iter().flat_map(identities).collect();
    let mut out = base;
    for peer in extra {
        // The first source wins, so a hand-written entry is never replaced by one that
        // merely turned up on the network.
        let ids = identities(&peer);
        if ids.iter().any(|id| seen.contains(id)) {
            continue;
        }
        seen.extend(ids);
        out.push(peer);
    }
    out
}

/// Do these two entries describe the same machine?
///
/// Matched on hostname or on any address a machine announced, so the cache updates an
/// entry rather than growing a second one when the same box is spelled two ways.
pub(crate) fn same_machine(a: &Peer, b: &Peer) -> bool {
    let theirs = identities(b);
    identities(a).iter().any(|id| theirs.contains(id))
}

/// The entries to publish in `peers.json`.
///
/// Only `config` peers are republished, and only when not hidden and not opted out.
/// That single rule keeps a fleet's topology deliberate — you publish the machines you
/// chose to list — bounds gossip to one hop, and keeps LAN addresses off a WAN host.
pub(crate) fn advertised(peers: &[Peer]) -> Vec<Peer> {
    peers
        .iter()
        .filter(|p| p.source.as_deref() == Some(SOURCE_CONFIG) && p.advertises())
        .cloned()
        .collect()
}

/// The document served at `/peers.json`, and read back from a peer.
///
/// The same table shape as `peers.toml`, so anything learned over federation can be
/// pasted straight into the hand-written file when you decide to keep it.
#[derive(Serialize, Deserialize, Debug, Default)]
pub(crate) struct PeerDoc {
    /// Always "server-splash"; lets a fetcher tell a dashboard from any other JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub peers: Vec<Peer>,
}

impl PeerDoc {
    pub(crate) const GENERATOR: &'static str = "server-splash";

    pub(crate) fn new(name: &str, version: &str, peers: Vec<Peer>) -> Self {
        PeerDoc {
            generator: Some(Self::GENERATOR.to_string()),
            version: Some(version.to_string()),
            name: Some(name.to_string()),
            peers,
        }
    }

    pub(crate) fn to_json(&self) -> Option<String> {
        serde_json::to_string_pretty(self).ok()
    }

    /// Parse a body fetched from a peer. Rejects a document that is not ours, so a
    /// portal or an error page served at the same path cannot inject peers.
    pub(crate) fn from_json(text: &str) -> Result<PeerDoc, String> {
        let doc: PeerDoc = serde_json::from_str(text).map_err(|e| e.to_string())?;
        match doc.generator.as_deref() {
            Some(Self::GENERATOR) => Ok(doc),
            other => Err(format!("not a server-splash peer list (generator = {other:?})")),
        }
    }
}

/// A starter `peers.toml`, written by `--scaffold-peers`.
///
/// It names the machines a user most plausibly has, commented out, so the file
/// documents its own shape without shipping links to machines that do not exist.
pub(crate) fn scaffold(host_suffix: &str, serve_port: u16) -> String {
    format!(
        r#"# Other machines running server-splash, listed on this machine's dashboard.
#
# Point `peers_file` in splash-config.toml (or SPLASH_PEERS) somewhere else to keep
# this list in a repo or a shared volume.
#
# `url` may be a full address (use https:// for WAN hosts, which also works through a
# reverse proxy) or just `host` / `host:port` — the scheme defaults to http and a bare
# name gets `{host_suffix}` appended, exactly as service links do.

# [[peer]]
# name = "ginger"
# url  = "ginger.local:{serve_port}"
# desc = "Dual RTX 3090 — media + GPUs"
# icon = "🫚"
# group = "LAN"

# [[peer]]
# name = "basil"
# url  = "https://basil.example.com"     # WAN: full https URL, no port needed
# desc = "Colo VPS — always on"
# group = "WAN"

# [[peer]]
# name = "old-thinkpad"
# url  = "192.168.1.50:{serve_port}"
# hidden = true                          # kept in the file, off the page
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_hostname_gets_a_scheme_and_the_host_suffix() {
        assert_eq!(normalize_url("ginger", ".local").as_deref(), Some("http://ginger.local"));
        assert_eq!(normalize_url("ginger.local", ".local").as_deref(), Some("http://ginger.local"));
    }

    #[test]
    fn a_port_survives_qualification() {
        // The suffix must be appended to the host, not to the host:port pair.
        assert_eq!(normalize_url("ginger:6420", ".local").as_deref(), Some("http://ginger.local:6420"));
        assert_eq!(
            normalize_url("ginger.local:6420", ".local").as_deref(),
            Some("http://ginger.local:6420")
        );
    }

    #[test]
    fn explicit_schemes_are_kept_and_https_is_not_downgraded() {
        assert_eq!(
            normalize_url("https://basil.example.com", ".local").as_deref(),
            Some("https://basil.example.com")
        );
        assert_eq!(
            normalize_url("HTTPS://basil.example.com:8443/x", ".local").as_deref(),
            Some("https://basil.example.com:8443/x"),
            "the scheme is lower-cased and the path kept"
        );
    }

    #[test]
    fn ips_and_paths_are_left_alone() {
        assert_eq!(
            normalize_url("192.168.1.72:6420", ".local").as_deref(),
            Some("http://192.168.1.72:6420")
        );
        assert_eq!(
            normalize_url("ginger.local/splash", ".local").as_deref(),
            Some("http://ginger.local/splash"),
            "a bare name keeps its path"
        );
        assert_eq!(normalize_url("ginger.local/", ".local").as_deref(), Some("http://ginger.local"));
    }

    #[test]
    fn an_empty_suffix_disables_qualification() {
        assert_eq!(normalize_url("ginger", "").as_deref(), Some("http://ginger"));
    }

    #[test]
    fn unusable_urls_are_refused_rather_than_guessed_at() {
        assert_eq!(normalize_url("", ".local"), None);
        assert_eq!(normalize_url("   ", ".local"), None);
        assert_eq!(normalize_url("ftp://ginger.local", ".local"), None, "only http(s) links a browser should follow");
        assert_eq!(normalize_url("http://", ".local"), None);
        assert_eq!(normalize_url("https:///only-a-path", ".local"), None);
    }

    #[test]
    fn an_ipv6_literal_is_not_mistaken_for_a_port() {
        assert_eq!(normalize_url("[::1]:6420", ".local").as_deref(), Some("http://[::1]:6420"));
        assert_eq!(normalize_url("[::1]", ".local").as_deref(), Some("http://[::1]"));
    }

    #[test]
    fn a_peers_file_is_read_in_order() {
        let text = r#"
[[peer]]
name = "ginger"
url = "ginger.local:6420"
desc = "GPU box"
group = "LAN"

[[peer]]
name = "basil"
url = "https://basil.example.com"
"#;
        let peers = parse(text, ".local").unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "ginger");
        assert_eq!(peers[0].url, "http://ginger.local:6420");
        assert_eq!(peers[0].desc_or_default(), "GPU box");
        assert_eq!(peers[0].display_target(), "ginger.local:6420");
        assert_eq!(peers[1].url, "https://basil.example.com");
        assert_eq!(peers[1].display_target(), "basil.example.com");
        assert_eq!(peers[1].desc_or_default(), "basil.example.com", "the host is the fallback desc");
        assert_eq!(peers[1].icon_or_default(), DEFAULT_PEER_ICON);
        assert!(peers[0].probes(), "status lights are on unless turned off");
    }

    #[test]
    fn a_peer_without_a_name_or_a_url_is_skipped_not_fatal() {
        let text = r#"
[[peer]]
url = "nameless.local:6420"

[[peer]]
name = "no-url"

[[peer]]
name = "ginger"
url = "ginger.local:6420"
"#;
        let peers = parse(text, ".local").unwrap();
        assert_eq!(peers.len(), 1, "only the complete entry survives");
        assert_eq!(peers[0].name, "ginger");
    }

    #[test]
    fn duplicate_urls_collapse_to_the_first_entry() {
        let text = r#"
[[peer]]
name = "ginger"
url = "ginger.local:6420"

[[peer]]
name = "ginger-again"
url = "http://GINGER.local:6420"
"#;
        let peers = parse(text, ".local").unwrap();
        assert_eq!(peers.len(), 1, "the same machine must not appear twice");
        assert_eq!(peers[0].name, "ginger");
    }

    #[test]
    fn hidden_and_probe_are_opt_in_flags() {
        let text = r#"
[[peer]]
name = "ginger"
url = "ginger.local:6420"
hidden = true
probe = false
"#;
        let peers = parse(text, ".local").unwrap();
        assert!(peers[0].is_hidden());
        assert!(!peers[0].probes());
    }

    #[test]
    fn an_empty_file_means_no_peers() {
        assert!(parse("", ".local").unwrap().is_empty());
        assert!(parse("# just a comment\n", ".local").unwrap().is_empty());
    }

    #[test]
    fn invalid_toml_is_an_error_the_caller_can_report() {
        assert!(parse("[[peer]\nname = ", ".local").is_err());
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("splash-peers-that-do-not-exist.toml");
        std::fs::remove_file(&path).ok();
        assert!(load(&path, ".local").is_empty());
    }

    #[test]
    fn the_scaffold_is_valid_and_all_comments() {
        let text = scaffold(".local", 6420);
        // Uncommenting nothing must parse to an empty list, and the text must be TOML.
        assert!(parse(&text, ".local").unwrap().is_empty());
        assert!(text.contains("[[peer]]"));
        assert!(text.contains("https://basil.example.com"), "it documents the WAN case");
    }

    #[test]
    fn two_spellings_of_one_machine_are_one_key() {
        assert_eq!(peer_key("http://ginger.local:6420/"), "ginger.local:6420");
        assert_eq!(peer_key("https://GINGER.local:6420"), "ginger.local:6420");
        assert_eq!(peer_key("ginger.local:6420/splash"), "ginger.local:6420");
        assert_eq!(peer_key("http://[::1]:6420"), "[::1]:6420");
    }

    #[test]
    fn a_dashboard_recognises_itself() {
        // The browse sees our own announcement, so this filter is what keeps a card
        // from linking to the page it is on.
        assert!(is_self("http://ginger.local:6420", "ginger.local", Some(6420)));
        assert!(is_self("http://ginger:6420", "ginger.local", Some(6420)), "a short host is the same host");
        assert!(is_self("http://ginger.local:6420/", "ginger.local.", Some(6420)));
        assert!(!is_self("http://ginger.local:6421", "ginger.local", Some(6420)), "another port is another page");
        assert!(!is_self("http://basil.local:6420", "ginger.local", Some(6420)));
        // Not serving: we cannot name our port, so any page on our hostname is ours.
        assert!(is_self("http://ginger.local:9999", "ginger.local", None));
        assert!(!is_self("http://ginger.local:6420", "", Some(6420)), "an unknown hostname matches nothing");
    }

    #[test]
    fn merging_keeps_the_entry_that_was_written_by_hand() {
        let curated = Peer {
            name: "ginger".into(),
            url: "http://ginger.local:6420".into(),
            desc: Some("curated".into()),
            source: Some(SOURCE_CONFIG.into()),
            ..Peer::default()
        };
        // The same machine as mDNS would announce it, under a different spelling.
        let heard = Peer {
            name: "GINGER".into(),
            url: "http://GINGER.local:6420/".into(),
            source: Some(SOURCE_MDNS.into()),
            ..Peer::default()
        };
        let new_machine = Peer {
            name: "basil".into(),
            url: "http://basil.local:6420".into(),
            source: Some(SOURCE_MDNS.into()),
            ..Peer::default()
        };

        let merged = merge(vec![curated], vec![heard, new_machine]);
        assert_eq!(merged.len(), 2, "the duplicate collapses, the new machine is added");
        assert_eq!(merged[0].desc.as_deref(), Some("curated"), "the hand-written entry wins");
        assert_eq!(merged[1].name, "basil");
    }

    #[test]
    fn a_machine_listed_by_ip_and_heard_by_name_is_one_card() {
        // The common LAN case: written down as an address, announces itself by hostname.
        let curated = Peer {
            name: "ginger".into(),
            url: "http://192.168.1.72:6420".into(),
            source: Some(SOURCE_CONFIG.into()),
            ..Peer::default()
        };
        let heard = Peer {
            name: "ginger.local".into(),
            url: "http://ginger.local:6420".into(),
            addrs: vec!["192.168.1.72".into()],
            source: Some(SOURCE_MDNS.into()),
            ..Peer::default()
        };

        let merged = merge(vec![curated], vec![heard]);
        assert_eq!(merged.len(), 1, "one machine is one card: {merged:?}");
        assert_eq!(merged[0].name, "ginger", "the entry you wrote is the one that shows");

        // A different address is a different machine, even with the same name shape.
        let elsewhere = Peer {
            name: "other".into(),
            url: "http://ginger.local:6421".into(),
            addrs: vec!["192.168.1.99".into()],
            ..Peer::default()
        };
        assert_eq!(merge(merged, vec![elsewhere]).len(), 2, "a different port is a different page");
    }

    #[test]
    fn only_configured_machines_are_published() {
        let config = Peer {
            name: "ginger".into(),
            url: "http://ginger.local:6420".into(),
            source: Some(SOURCE_CONFIG.into()),
            ..Peer::default()
        };
        let discovered = Peer {
            name: "alpha".into(),
            url: "http://alpha.local:6420".into(),
            source: Some(SOURCE_MDNS.into()),
            ..Peer::default()
        };
        let learned = Peer {
            name: "carol".into(),
            url: "http://carol.local:6420".into(),
            source: Some(SOURCE_FEDERATION.into()),
            ..Peer::default()
        };
        let hidden = Peer {
            name: "retired".into(),
            url: "http://retired.local:6420".into(),
            source: Some(SOURCE_CONFIG.into()),
            hidden: Some(true),
            ..Peer::default()
        };
        let opted_out = Peer {
            name: "private".into(),
            url: "http://private.local:6420".into(),
            source: Some(SOURCE_CONFIG.into()),
            advertise: Some(false),
            ..Peer::default()
        };

        let published = advertised(&[config, discovered, learned, hidden, opted_out]);
        assert_eq!(published.len(), 1, "only a machine we wrote down and did not hide: {published:?}");
        assert_eq!(published[0].name, "ginger");
    }

    #[test]
    fn a_peer_list_round_trips_through_json() {
        let peers = parse(
            "[[peer]]\nname = \"ginger\"\nurl = \"ginger.local:6420\"\ngroup = \"LAN\"\n",
            ".local",
        )
        .unwrap();
        let doc = PeerDoc::new("ginger.local", "0.1.0", peers);
        let json = doc.to_json().unwrap();

        assert!(json.contains("\"generator\": \"server-splash\""), "{json}");
        assert!(!json.contains("null"), "the published form does not carry empty fields: {json}");
        assert!(!json.contains("source"), "provenance is this run's, not the file's: {json}");

        let back = PeerDoc::from_json(&json).unwrap();
        assert_eq!(back.name.as_deref(), Some("ginger.local"));
        assert_eq!(back.peers.len(), 1);
        assert_eq!(back.peers[0].url, "http://ginger.local:6420");
        assert_eq!(back.peers[0].group.as_deref(), Some("LAN"));
    }

    #[test]
    fn a_foreign_document_is_not_a_peer_list() {
        // A captive portal, a proxy error page or an unrelated service answering the
        // same path must not be able to inject peers into the page.
        assert!(PeerDoc::from_json(r#"{"generator":"something-else","peers":[]}"#).is_err());
        assert!(PeerDoc::from_json(r#"{"peers":[]}"#).is_err(), "no generator is not ours either");
        assert!(PeerDoc::from_json(r#"{"generator":"server-splash","peers":[]}"#).is_ok());
        assert!(PeerDoc::from_json("<html>404</html>").is_err());
    }
}

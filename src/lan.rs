//! mDNS / DNS-SD discovery: other dashboards on the same network, found without
//! anybody typing an address.
//!
//! This is the "ping other splash pages" idea, with one correction: it cannot happen
//! in the page. A browser has no mDNS API and cannot multicast, so a web page can
//! never find a sibling page — the discovery has to run in this process and be baked
//! into the next regeneration. That is why `--serve` (a long-running daemon) gets live
//! discovery and a one-shot `--auto` gets a short listen.
//!
//! We register `_splash._tcp.local` and browse the same type — the mechanism printers,
//! Chromecast and Home Assistant have used for years. It is limited to one layer-2
//! broadcast domain: mDNS does not cross a router, a VLAN boundary, or most VPNs. That
//! is what the HTTP federation in `federation.rs` is for.
//!
//! What we announce is deliberately small: the hostname, the page title and a version.
//! No service list — the discovery record is the *address*, not an inventory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::peers::{self, Peer, SOURCE_MDNS};

/// The DNS-SD service type. Ends in `.local.` because that is the mDNS domain.
pub(crate) const SERVICE_TYPE: &str = "_splash._tcp.local.";

/// What this machine tells the LAN about itself.
#[derive(Debug, Clone)]
pub(crate) struct Announce {
    /// The dashboard's hostname, already qualified (`ginger.local`).
    pub name: String,
    /// The page title, shown as the card's description on other dashboards.
    pub title: String,
    /// The port its page is served on.
    pub port: u16,
}

struct Seen {
    /// The mDNS fullname, so a removal event can find the entry again.
    fullname: String,
    peer: Peer,
    last: Instant,
}

/// A live mDNS handle: announces this dashboard (optionally) and remembers the ones
/// it hears about.
///
/// Cloning is cheap and every clone shares the same view, so the regeneration thread
/// and the server thread can both hold one.
#[derive(Clone)]
pub(crate) struct Lan {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// A keep-alive handle: dropping the last clone stops the daemon's own thread, so
    /// this field exists to be held, not read.
    #[allow(dead_code)]
    daemon: ServiceDaemon,
}

impl Lan {
    /// Join the multicast group, optionally announcing this dashboard.
    ///
    /// Returns `None` when mDNS is unavailable — no network, port 5353 already taken by
    /// a daemon that will not share, a sandbox with no multicast. Discovery is a bonus,
    /// so every failure here is a note and a page without peers, never a failed run.
    pub(crate) fn start(announce: Option<&Announce>) -> Option<Lan> {
        let daemon = match ServiceDaemon::new() {
            Ok(daemon) => daemon,
            Err(e) => {
                eprintln!("  \x1b[33mnote\x1b[0m: LAN discovery unavailable ({e})");
                return None;
            }
        };

        if let Some(info) = announce {
            register(&daemon, info);
        }

        let receiver = match daemon.browse(SERVICE_TYPE) {
            Ok(receiver) => receiver,
            Err(e) => {
                eprintln!("  \x1b[33mnote\x1b[0m: LAN discovery unavailable ({e})");
                return None;
            }
        };

        let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::clone(&seen);
        std::thread::spawn(move || {
            // Ends when the daemon shuts down, which drops the sender.
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(service) => {
                        let mut props = HashMap::new();
                        for key in ["name", "title"] {
                            if let Some(value) = service.get_property_val_str(key) {
                                props.insert(key.to_string(), value.to_string());
                            }
                        }
                        // Sorted so the fallback address does not change between runs.
                        let mut v4: Vec<String> =
                            service.get_addresses_v4().iter().map(|ip| ip.to_string()).collect();
                        v4.sort();

                        let Some(peer) = peer_from(
                            service.get_hostname(),
                            service.get_port(),
                            &props,
                            &v4,
                        ) else {
                            continue;
                        };

                        let fullname = service.get_fullname().to_string();
                        if let Ok(mut seen) = store.lock() {
                            seen.retain(|s| s.fullname != fullname);
                            seen.push(Seen { fullname, peer, last: Instant::now() });
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        // A goodbye packet: the machine shut down cleanly, so drop it
                        // rather than waiting out the TTL.
                        if let Ok(mut seen) = store.lock() {
                            seen.retain(|s| s.fullname != fullname);
                        }
                    }
                    _ => {}
                }
            }
        });

        Some(Lan { seen, daemon })
    }

    /// Browse without announcing, for a one-shot run that only wants to see who is out
    /// there. A page generated by a timer has no business claiming a service name that
    /// disappears seconds later.
    pub(crate) fn browse_only() -> Option<Lan> {
        Lan::start(None)
    }

    /// Peers heard from within `ttl`, de-duplicated by address.
    pub(crate) fn peers(&self, ttl: Duration) -> Vec<Peer> {
        let Ok(mut seen) = self.seen.lock() else { return Vec::new() };
        seen.retain(|s| s.last.elapsed() < ttl);
        let peers: Vec<Peer> = seen.iter().map(|s| s.peer.clone()).collect();
        // The same machine can answer on two interfaces with different fullnames.
        peers::merge(Vec::new(), peers)
    }

    /// Block for a moment so a short-lived browse can actually hear an answer.
    pub(crate) fn listen(&self, wait: Duration) {
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }
}

/// Build the peer card for a dashboard someone announced.
///
/// Split out from the event loop so it can be tested without a network: everything it
/// needs arrives as arguments.
///
/// The URL prefers the announced *hostname* over a resolved IP. An IP is tempting
/// because we know one answered, but a machine resolves to every address it has —
/// including `docker0`, a Tailscale range, or an IPv6 link-local — and a link to the
/// wrong one fails in a way nobody can debug from the card. The hostname goes through
/// the same `.local` resolution every other link on this dashboard already needs.
pub(crate) fn peer_from(
    host: &str,
    port: u16,
    props: &HashMap<String, String>,
    addrs: &[String],
) -> Option<Peer> {
    if port == 0 {
        return None;
    }
    let host = host.trim().trim_end_matches('.');

    let url_host = if host.is_empty() {
        addrs.first()?.to_string()
    } else {
        host.to_string()
    };
    if url_host.is_empty() {
        return None;
    }

    let name = props
        .get("name")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| short_name(&url_host));

    let desc = props
        .get("title")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let peer = Peer {
        name,
        url: format!("http://{url_host}:{port}"),
        desc,
        // Carried so a machine also listed by IP in `peers.toml` collapses into one card.
        addrs: addrs.to_vec(),
        // Never republished: we did not choose this machine, the network did.
        advertise: Some(false),
        ..Peer::default()
    };
    peers::normalize_one(peer, "", SOURCE_MDNS)
}

/// `ginger.local` -> `ginger`, for a card title when the announcer sent none.
fn short_name(host: &str) -> String {
    host.split('.').next().unwrap_or(host).to_string()
}

/// The DNS host record for a hostname: `ginger` -> `ginger.local.`.
fn service_host(name: &str) -> String {
    let name = name.trim();
    if name.ends_with(".local.") {
        name.to_string()
    } else if name.ends_with(".local") {
        format!("{name}.")
    } else {
        format!("{name}.local.")
    }
}

/// Advertise this dashboard. Best effort: a machine that cannot announce still serves
/// its page, and still browses for everyone else.
fn register(daemon: &ServiceDaemon, info: &Announce) {
    let host = service_host(&info.name);
    let version = env!("CARGO_PKG_VERSION");
    let properties = [
        ("name", info.name.as_str()),
        ("title", info.title.as_str()),
        ("v", version),
    ];

    // Empty addresses plus `enable_addr_auto` means "every address this host has",
    // which keeps working when DHCP hands it a new one.
    let service = match ServiceInfo::new(
        SERVICE_TYPE,
        &info.name,
        &host,
        "",
        info.port,
        &properties[..],
    ) {
        Ok(service) => service.enable_addr_auto(),
        Err(e) => {
            eprintln!("  \x1b[33mnote\x1b[0m: cannot announce this dashboard on the LAN ({e})");
            return;
        }
    };

    if let Err(e) = daemon.register(service) {
        eprintln!("  \x1b[33mnote\x1b[0m: cannot announce this dashboard on the LAN ({e})");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn ip(addrs: &[&str]) -> Vec<String> {
        addrs.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn an_announced_dashboard_becomes_a_peer_card() {
        let peer = peer_from(
            "ginger.local.",
            6420,
            &props(&[("name", "ginger.local"), ("title", "Home Server")]),
            &ip(&["192.168.1.72"]),
        )
        .expect("a resolved service is a usable peer");

        assert_eq!(peer.name, "ginger.local");
        assert_eq!(peer.url, "http://ginger.local:6420", "the hostname is preferred over the IP");
        assert_eq!(peer.desc.as_deref(), Some("Home Server"));
        assert_eq!(peer.source.as_deref(), Some(SOURCE_MDNS));
        assert_eq!(peer.advertise, Some(false), "a peer we merely heard must not be re-advertised");
    }

    #[test]
    fn a_missing_title_falls_back_to_the_host() {
        let peer = peer_from("basil.local.", 6420, &props(&[]), &[]).unwrap();
        assert_eq!(peer.desc, None, "no invented description");
        assert_eq!(peer.desc_or_default(), "basil.local:6420", "the card falls back to the address");
    }

    #[test]
    fn a_missing_name_is_derived_from_the_hostname() {
        let peer = peer_from("basil.local.", 6420, &props(&[]), &[]).unwrap();
        assert_eq!(peer.name, "basil", "the short host is the best available name");
    }

    #[test]
    fn a_hostname_less_answer_falls_back_to_its_address() {
        let peer = peer_from("", 8080, &props(&[]), &ip(&["192.168.1.9"])).unwrap();
        assert_eq!(peer.url, "http://192.168.1.9:8080");
        assert_eq!(peer.name, "192", "short_name splits on the first dot");
    }

    #[test]
    fn an_unusable_answer_is_ignored() {
        assert!(peer_from("ginger.local.", 0, &props(&[]), &[]).is_none(), "no port, no page");
        assert!(peer_from("", 6420, &props(&[]), &[]).is_none(), "no host and no address");
    }

    #[test]
    fn the_host_record_is_always_fully_qualified() {
        assert_eq!(service_host("ginger"), "ginger.local.");
        assert_eq!(service_host("ginger.local"), "ginger.local.");
        assert_eq!(service_host("ginger.local."), "ginger.local.");
        assert_eq!(service_host("ginger.example.com"), "ginger.example.com.local.");
    }

    #[test]
    fn a_service_type_in_dns_sd_form() {
        // The trailing dot is required by the crate, and a bare label is rejected.
        assert!(SERVICE_TYPE.starts_with('_'));
        assert!(SERVICE_TYPE.ends_with("._tcp.local."));
        assert!(SERVICE_TYPE.len() < 16 + "._tcp.local.".len(), "RFC 6763 caps the label at 15 bytes");
    }
}

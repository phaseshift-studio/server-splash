//! Everything this dashboard knows about other machines, assembled on demand.
//!
//! Two callers need the same answer: the page generator ("who belongs on this page?") and
//! the live endpoint behind the **Resync with server** button ("who can you see *right
//! now*?"). Keeping the assembly in one place is what stops the page and the button
//! disagreeing about which sources exist or in what order they win.
//!
//! The sources, nearest first:
//!
//! 1. `peers.toml` — authoritative, and the only thing ever republished.
//! 2. mDNS — machines announcing themselves on this network right now.
//! 3. the page's own memory — machines seen on an earlier run, tried again every time.
//! 4. federation — machines a reachable peer told us about.
//!
//! A run with no long-lived browser attached does exactly the same work, which is why the
//! Resync button and a regeneration always produce the same cards.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::cache;
use crate::config::SplashConfig;
use crate::federation;
use crate::lan::Lan;
use crate::peers::{self, Peer};

/// Where this server publishes the live peer list.
///
/// Baked into the page so the Resync button knows where to ask. Served only by `splash`
/// itself; when the page is deployed to nginx or Apache the request 404s and the button
/// quietly falls back to what the page was built with.
pub(crate) const LIVE_PATH: &str = "/_splash/peers";

/// What the collection found, and how much each source contributed.
///
/// The counts are for the console line the generator prints; the cards and the memory are
/// what the page needs.
#[derive(Debug, Default)]
pub(crate) struct Collected {
    /// Cards for the page, nearest source first.
    pub cards: Vec<Peer>,
    /// Machines the page should remember for next time.
    pub memory: Vec<Peer>,
    pub configured: usize,
    pub discovered: usize,
    pub remembered: usize,
    pub learned: usize,
}

/// Gather every source into one list.
///
/// `federated` is off for the first paint of `--serve`, so one unreachable WAN peer cannot
/// delay a page that is otherwise ready; the worker turns it on for every later pass.
pub(crate) fn collect(
    cfg: &SplashConfig,
    lan: Option<&Lan>,
    hostname: &str,
    federated: bool,
) -> Collected {
    let mut collected = Collected::default();

    // 1. The hand-written list.
    let mut cards = peers::load(&cfg.get_peers_path(), &cfg.host_suffix());
    let configured_peers = cards.clone();
    collected.configured = configured_peers.len();

    // Everything this run learned from the network, so it can be remembered next time.
    let mut observed: Vec<Peer> = Vec::new();

    // 2. Whoever is announcing itself here. Our own announcement comes back through the
    // same browse, so a dashboard that did not filter itself would offer a card to the
    // page you are on.
    if let Some(lan) = lan {
        let heard: Vec<Peer> = lan
            .peers(Duration::from_secs(cfg.peer_ttl_secs()))
            .into_iter()
            .filter(|p| !peers::is_self(&p.url, hostname, cfg.serve_port()))
            .collect();
        observed.extend(heard.iter().cloned());
        let before = cards.len();
        cards = peers::merge(cards, heard);
        collected.discovered = cards.len() - before;
    }

    // 3. What the page remembers. Read on every run, not only when the network fails: a
    // machine that was here last week is worth one cheap request.
    let cache_days = cfg.peer_cache_days();
    let cache_max_age = Duration::from_secs(cache_days.saturating_mul(86_400));
    let hints: Vec<Peer> = if cache_days > 0 {
        let output_dir = PathBuf::from(cfg.get_output_dir("./server-splash"));
        let mut memory = cache::read(&output_dir.join(cfg.get_output_file()), cache_max_age);
        if memory.is_empty() {
            // Generating into a scratch directory and deploying elsewhere is a supported
            // shape; the last deployed page carries the memory in that case.
            if let Some(deploy) = cfg.deploy_path.as_deref().filter(|d| !d.trim().is_empty()) {
                memory = cache::read(&Path::new(deploy).join(cfg.get_output_file()), cache_max_age);
            }
        }
        // Filtered like every other source: a page written under a different hostname must
        // not offer a card to itself.
        memory
            .into_iter()
            .filter(|p| !peers::is_self(&p.url, hostname, cfg.serve_port()))
            .collect()
    } else {
        Vec::new()
    };

    // 4. What the machines we can ask know. Seeds are the configured and remembered
    // machines only: one already announcing itself has nothing to tell us, and would be
    // marked down without anybody trying it.
    let seeds = peers::merge(configured_peers, hints.clone());
    let federating = federated && cfg.federation_enabled();
    let pulled = if federating {
        federation::pull(
            &seeds,
            &cfg.host_suffix(),
            hostname,
            cfg.serve_port(),
            Duration::from_millis(cfg.http_timeout_ms()),
        )
    } else {
        federation::Pulled::default()
    };

    let before = cards.len();
    cards = peers::merge(cards, hints.clone());
    collected.remembered = cards.len() - before;

    let before = cards.len();
    cards = peers::merge(cards, pulled.learned.clone());
    collected.learned = cards.len() - before;

    // Did the machine answer? Recorded so the page shows it before any script runs; a card
    // nobody asked about stays unknown and the browser decides.
    if federating {
        mark_reachability(&mut cards, &seeds, &pulled.answered);
    }

    // Fold the findings into the memory that goes into the new page. Configured machines
    // are never remembered: they are already in `peers.toml`, and removing one from there
    // should be the end of it.
    observed.extend(pulled.learned.iter().cloned());
    observed.extend(
        pulled
            .answered
            .iter()
            .filter(|p| p.source.as_deref() == Some(peers::SOURCE_CACHE))
            .cloned(),
    );
    collected.memory = if cache_days > 0 {
        cache::merge(&hints, &observed, cache_max_age)
    } else {
        Vec::new()
    };
    collected.cards = cards;
    collected
}

/// Record, on each card we actually asked, whether the machine answered.
///
/// Only a configured or remembered machine is asked, so only those get a verdict; a card
/// discovered over mDNS or learned from a peer keeps `None`, and the browser's probe fills
/// it in.
fn mark_reachability(peers: &mut [Peer], seeds: &[Peer], answered: &[Peer]) {
    for peer in peers.iter_mut() {
        if seeds.iter().any(|seed| peers::same_machine(seed, peer)) {
            peer.alive = Some(answered.iter().any(|a| peers::same_machine(a, peer)));
        }
    }
}

/// What the running server needs to answer "who can you see right now?".
///
/// Cloned per request: the config, the live mDNS browse, and the hostname the page was
/// generated with (used to keep this machine off its own list).
#[derive(Clone)]
pub(crate) struct Context {
    cfg: SplashConfig,
    lan: Option<Lan>,
    hostname: String,
    /// One assembly at a time. Two tabs clicking Resync together should not each launch
    /// their own round of federation requests.
    gate: Arc<Mutex<()>>,
}

impl Context {
    pub(crate) fn new(cfg: &SplashConfig, lan: Option<Lan>, hostname: &str) -> Self {
        Context {
            cfg: cfg.clone(),
            lan,
            hostname: hostname.to_string(),
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// The cards the page can insert, as JSON.
    ///
    /// Pre-rendered HTML rather than a list of fields: the button then draws a card
    /// exactly the way the generator does — same tags, same escaping, same status classes
    /// — and there is one renderer to keep correct instead of two.
    pub(crate) fn peers_json(&self) -> String {
        // Held for the duration of the assembly; a poisoned lock just means the previous
        // request panicked, which is not a reason to refuse this one.
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());

        let collected = collect(&self.cfg, self.lan.as_ref(), &self.hostname, true);
        let cards: Vec<serde_json::Value> = collected
            .cards
            .iter()
            .filter(|p| !p.is_hidden())
            .map(|p| {
                serde_json::json!({
                    "url": p.url,
                    "html": crate::generator::render_peer_card(p, self.cfg.inline_icons()),
                })
            })
            .collect();

        serde_json::json!({
            "generator": crate::peers::PeerDoc::GENERATOR,
            "version": env!("CARGO_PKG_VERSION"),
            "cards": cards,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("splash-live-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(dir: &Path, peers_file: Option<&Path>) -> SplashConfig {
        SplashConfig {
            peers_file: peers_file.map(|p| p.to_string_lossy().to_string()),
            output_dir: Some(dir.to_string_lossy().to_string()),
            federation: Some(false),
            no_agent: Some(true),
            ..SplashConfig::default()
        }
    }

    #[test]
    fn an_unconfigured_dashboard_sees_nothing_and_remembers_nothing() {
        let dir = temp_dir("empty");
        let cfg = config(&dir, None);

        let collected = collect(&cfg, None, "basil.local", false);
        assert!(collected.cards.is_empty());
        assert!(collected.memory.is_empty(), "no machines, nothing to remember");
        assert_eq!(collected.configured, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn configured_machines_are_not_remembered() {
        let dir = temp_dir("configured");
        let peers_toml = dir.join("peers.toml");
        std::fs::write(&peers_toml, "[[peer]]\nname = \"ginger\"\nurl = \"ginger.local:6420\"\n").unwrap();
        let cfg = config(&dir, Some(&peers_toml));

        let collected = collect(&cfg, None, "basil.local", false);
        assert_eq!(collected.cards.len(), 1);
        assert_eq!(collected.cards[0].name, "ginger");
        assert_eq!(collected.configured, 1);
        assert!(
            collected.memory.is_empty(),
            "a machine in peers.toml is already on disk; remembering it would outlive its removal"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_live_endpoint_renders_the_same_cards_the_page_would() {
        let dir = temp_dir("json");
        let peers_toml = dir.join("peers.toml");
        std::fs::write(
            &peers_toml,
            "[[peer]]\nname = \"ginger\"\nurl = \"ginger.local:6420\"\ndesc = \"GPU box\"\ngroup = \"LAN\"\n",
        )
        .unwrap();
        let cfg = config(&dir, Some(&peers_toml));

        let json = Context::new(&cfg, None, "basil.local").peers_json();
        assert!(json.contains("\"generator\":\"server-splash\""), "{json}");
        assert!(json.contains("ginger.local:6420"), "{json}");
        assert!(json.contains("data-peer=\\\"ginger\\\""), "a card, not just a name: {json}");
        assert!(json.contains("peer-tag"), "the same tag the generator draws: {json}");
    }

    #[test]
    fn the_endpoint_leaves_out_a_hidden_machine() {
        let dir = temp_dir("hidden");
        let peers_toml = dir.join("peers.toml");
        std::fs::write(
            &peers_toml,
            "[[peer]]\nname = \"secret\"\nurl = \"secret.local:6420\"\nhidden = true\n",
        )
        .unwrap();
        let cfg = config(&dir, Some(&peers_toml));

        let json = Context::new(&cfg, None, "basil.local").peers_json();
        assert!(!json.contains("secret"), "hidden stays hidden: {json}");
        assert!(json.contains("\"cards\":[]"), "{json}");
    }

    #[test]
    fn only_asked_machines_get_a_reachability_verdict() {
        let up = Peer {
            name: "up".into(),
            url: "http://up.local:6420".into(),
            source: Some(peers::SOURCE_CONFIG.into()),
            ..Peer::default()
        };
        let down = Peer {
            name: "down".into(),
            url: "http://down.local:6420".into(),
            source: Some(peers::SOURCE_CONFIG.into()),
            ..Peer::default()
        };
        let heard = Peer {
            name: "heard".into(),
            url: "http://heard.local:6420".into(),
            source: Some(peers::SOURCE_MDNS.into()),
            ..Peer::default()
        };

        let seeds = vec![up.clone(), down.clone()];
        let answered = vec![up.clone()];
        let mut cards = vec![up, down, heard];
        mark_reachability(&mut cards, &seeds, &answered);

        assert_eq!(cards[0].alive, Some(true), "a seed that replied is up");
        assert_eq!(cards[1].alive, Some(false), "a seed that did not reply is down");
        assert_eq!(cards[2].alive, None, "a machine nobody asked stays unknown");
    }
}

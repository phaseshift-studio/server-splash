//! Learning about machines mDNS cannot reach.
//!
//! mDNS stops at the router, so a box in a datacentre or on another VLAN is invisible
//! to it. But every dashboard already knows a few machines — by hand, or from the cache
//! of what it saw last time — and those machines know a few more. Asking a peer for its
//! list turns the addresses we have into seeds for the rest of the fleet, over plain
//! HTTP, which works across subnets, VPNs and the public internet.
//!
//! Two rules keep this from becoming an open relay:
//!
//! 1. **Only configured or remembered peers are fetched from.** A machine discovered over
//!    mDNS is not a seed, so learning cannot chase its own tail around the network.
//! 2. **Only configured peers are republished** (see `peers::advertised`). What we read
//!    here is shown on this page and stops there, which bounds the flood to one hop:
//!    ginger teaches us about basil, but basil does not learn about carol through us.
//!
//! Everything is a plain `GET /peers.json` of a JSON document. No auth, no writes, no
//! side effects — a dashboard that is publicly readable can be federated from, and one
//! that is not simply does not answer.

use std::time::Duration;

use crate::peers::{self, Peer, PeerDoc, SOURCE_FEDERATION};

/// More seeds than this in one run is a list nobody curated. The cap also bounds the
/// number of threads a single refresh can spawn.
const MAX_SEEDS: usize = 16;

/// Where a dashboard publishes its peer list.
pub(crate) fn peers_url(base: &str) -> String {
    format!("{}/peers.json", base.trim_end_matches('/'))
}

/// What one pull produced.
#[derive(Debug, Default)]
pub(crate) struct Pulled {
    /// Machines the seeds knew about.
    pub learned: Vec<Peer>,
    /// The seeds that answered. Proof they are still there, which the page shows and the
    /// cache uses to keep their remembered timestamp fresh.
    pub answered: Vec<Peer>,
}

/// A seed worth a fetch: one the user wrote down, or one the cache remembers.
///
/// A machine heard over mDNS is deliberately not a seed — it is on this network already,
/// so there is little to learn from it, and using it would let learning wander.
fn trusted_seed(peer: &Peer) -> bool {
    matches!(
        peer.source.as_deref(),
        Some(peers::SOURCE_CONFIG) | Some(peers::SOURCE_CACHE)
    )
}

/// Fetch the peer lists of `seeds` and return the machines they know about.
///
/// Seeds are fetched concurrently: a remembered machine that is switched off costs one
/// timeout in total, not one each, so consulting the cache on every run stays cheap.
/// Failures are expected and silent — a peer being down is the normal state of a peer
/// you have not visited in a while.
pub(crate) fn pull(
    seeds: &[Peer],
    host_suffix: &str,
    own_host: &str,
    own_port: Option<u16>,
    timeout: Duration,
) -> Pulled {
    let client = match reqwest::blocking::Client::builder().timeout(timeout).build() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("  \x1b[33mnote\x1b[0m: cannot build an HTTP client for peer learning ({e})");
            return Pulled::default();
        }
    };

    let tried: Vec<&Peer> = seeds.iter().filter(|s| trusted_seed(s)).take(MAX_SEEDS).collect();
    if tried.is_empty() {
        return Pulled::default();
    }

    let results: Vec<Option<(Peer, Vec<Peer>)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = tried
            .iter()
            .map(|seed| {
                let client = &client;
                scope.spawn(move || fetch_list(client, seed, host_suffix, own_host, own_port))
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or(None))
            .collect()
    });

    let mut pulled = Pulled::default();
    for (seed, learned) in results.into_iter().flatten() {
        pulled.answered.push(seed);
        pulled.learned.extend(learned);
    }
    pulled
}

/// One seed, one request. `None` means it did not answer usefully.
fn fetch_list(
    client: &reqwest::blocking::Client,
    seed: &Peer,
    host_suffix: &str,
    own_host: &str,
    own_port: Option<u16>,
) -> Option<(Peer, Vec<Peer>)> {
    let response = client.get(peers_url(&seed.url)).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().ok()?;
    let doc = PeerDoc::from_json(&body).ok()?;

    let publisher = doc
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&seed.name)
        .to_string();

    let mut learned = Vec::new();
    for remote in doc.peers {
        let Some(mut peer) = peers::normalize_one(remote, host_suffix, SOURCE_FEDERATION) else {
            continue;
        };
        // A peer's list may include the machine reading it, or this very host.
        if peers::is_self(&peer.url, own_host, own_port) || peers::same_machine(&peer, seed) {
            continue;
        }
        peer.learned_from = Some(publisher.clone());
        learned.push(peer);
    }

    Some((seed.clone(), learned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(name: &str, url: &str, source: &str) -> Peer {
        Peer {
            name: name.into(),
            url: url.into(),
            source: Some(source.into()),
            ..Peer::default()
        }
    }

    #[test]
    fn the_list_lives_at_a_well_known_path() {
        assert_eq!(peers_url("http://ginger.local:6420"), "http://ginger.local:6420/peers.json");
        assert_eq!(peers_url("https://basil.example.com/"), "https://basil.example.com/peers.json");
        assert_eq!(
            peers_url("https://basil.example.com/splash"),
            "https://basil.example.com/splash/peers.json",
            "a dashboard on a subpath keeps it"
        );
    }

    #[test]
    fn configured_and_remembered_machines_are_worth_a_fetch() {
        assert!(trusted_seed(&seed("ginger", "http://ginger.local:6420", peers::SOURCE_CONFIG)));
        assert!(trusted_seed(&seed("ginger", "http://ginger.local:6420", peers::SOURCE_CACHE)));
        assert!(
            !trusted_seed(&seed("alpha", "http://alpha.local:6420", peers::SOURCE_MDNS)),
            "something already announcing itself locally has nothing to teach us"
        );
        assert!(!trusted_seed(&seed("carol", "http://carol.local:6420", peers::SOURCE_FEDERATION)));
        assert!(!trusted_seed(&Peer::default()));
    }

    #[test]
    fn a_seed_and_this_host_are_not_learned() {
        // The filter that runs inside fetch_list(), exercised without a network.
        let remote = PeerDoc::from_json(
            r#"{"generator":"server-splash","name":"ginger.local","peers":[
                 {"name":"self","url":"http://ginger.local:6420"},
                 {"name":"me","url":"http://basil.local:6420"},
                 {"name":"carol","url":"http://carol.local:6420"}
               ]}"#,
        )
        .unwrap();

        let seed = seed("ginger.local", "http://ginger.local:6420", peers::SOURCE_CONFIG);
        let mut kept = Vec::new();
        for p in remote.peers {
            let Some(peer) = peers::normalize_one(p, ".local", SOURCE_FEDERATION) else { continue };
            if peers::is_self(&peer.url, "basil.local", Some(6420))
                || peers::same_machine(&peer, &seed)
            {
                continue;
            }
            kept.push(peer);
        }
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "carol");
    }

    #[test]
    fn a_pull_with_nothing_to_ask_does_nothing() {
        let pulled = pull(&[], ".local", "basil.local", Some(6420), Duration::from_millis(1));
        assert!(pulled.learned.is_empty() && pulled.answered.is_empty());
    }
}

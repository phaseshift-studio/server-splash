//! A memory of machines seen before, carried inside the page itself.
//!
//! Discovery is good until it isn't: a switch stops passing multicast, a laptop is shut,
//! the network is not yours. The page therefore remembers what it found, in a comment at
//! the end of itself — `<!-- CACHE {…} -->` — and the next generation reads it back out of
//! the page it is about to replace.
//!
//! Keeping it in the page rather than beside it means there is no cache file, no cache
//! directory, and nothing to clean up: the artefact and its memory are the same object. It
//! also means the memory travels with the page when it is deployed, so a machine that
//! generates into a temporary directory and copies the result still remembers.
//!
//! The rules are the same as they would be for a file:
//!
//! 1. **Remembered machines are tried, not trusted.** Each shows its age (`last seen 3h
//!    ago`) and whether it answered just now; the address is a candidate.
//! 2. **Nothing is republished.** Only `peers.toml` entries go into `/peers.json`, so a
//!    machine cannot enter the fleet's published topology by being remembered.
//! 3. **Entries expire** after `peer_cache_days`, so a retired machine stops being tried.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::peers::{self, Peer, SOURCE_CACHE};

/// The marker. Written with a space and a JSON body, read back by `read`.
pub(crate) const MARK: &str = "<!-- CACHE ";

/// The page's memory: a list of peers, each with the epoch second it was last seen.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct Memory {
    #[serde(default)]
    peers: Vec<Peer>,
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the memory out of an existing page, dropping anything older than `max_age`.
///
/// A page that does not exist, has no marker, or carries one we cannot parse simply has
/// no memory — that is the first run, and it must not be an error.
pub(crate) fn read(page: &Path, max_age: Duration) -> Vec<Peer> {
    read_at(page, max_age, now_epoch())
}

fn read_at(page: &Path, max_age: Duration, now: u64) -> Vec<Peer> {
    let Ok(html) = std::fs::read_to_string(page) else { return Vec::new() };
    // `rfind`: if a page ever ended up nested inside another, the last marker is the one
    // this page wrote.
    let Some(start) = html.rfind(MARK) else { return Vec::new() };
    let rest = &html[start + MARK.len()..];
    let Some(end) = rest.find("-->") else { return Vec::new() };

    let Ok(memory) = serde_json::from_str::<Memory>(rest[..end].trim()) else {
        return Vec::new();
    };

    let limit = max_age.as_secs();
    let mut peers: Vec<Peer> = memory
        .peers
        .into_iter()
        .filter(|p| match p.last_seen {
            // No timestamp: written by an older version, so give it the benefit of the
            // doubt once and let this run stamp it.
            None => true,
            Some(seen) => now.saturating_sub(seen) <= limit,
        })
        .map(|mut p| {
            p.source = Some(SOURCE_CACHE.to_string());
            p
        })
        .collect();
    peers.sort_by_key(|p| p.name.to_lowercase());
    peers
}

/// Fold this run's observations into the previous memory and return what to write back.
///
/// Observed entries get a fresh timestamp; entries not observed keep theirs, so their age
/// keeps growing and they expire on their own schedule.
pub(crate) fn merge(previous: &[Peer], observed: &[Peer], max_age: Duration) -> Vec<Peer> {
    merge_at(previous, observed, max_age, now_epoch())
}

fn merge_at(previous: &[Peer], observed: &[Peer], max_age: Duration, now: u64) -> Vec<Peer> {
    let mut entries: Vec<Peer> = previous.to_vec();

    for peer in observed {
        let mut fresh = peer.clone();
        fresh.last_seen = Some(now);
        match entries.iter_mut().find(|e| peers::same_machine(e, &fresh)) {
            Some(existing) => {
                // The newest spelling of the address wins: a machine that changed its
                // DHCP lease, or appeared by name after being written down by IP, should
                // be tried where it was last seen.
                existing.name = fresh.name;
                existing.url = fresh.url;
                existing.desc = fresh.desc;
                existing.group = fresh.group;
                existing.icon = fresh.icon;
                existing.addrs = fresh.addrs;
                existing.last_seen = Some(now);
            }
            None => entries.push(fresh),
        }
    }

    let limit = max_age.as_secs();
    entries.retain(|p| match p.last_seen {
        None => true,
        Some(seen) => now.saturating_sub(seen) <= limit,
    });
    entries.sort_by_key(|p| p.name.to_lowercase());
    entries
}

/// The comment to carry in the page, or nothing when there is nothing to remember.
///
/// `--` is escaped as `-\u002d` because a JSON string is the only place two hyphens can
/// appear, and an unescaped `-->` in a value would end the comment early and dump the rest
/// of the memory into the document as visible text.
pub(crate) fn comment(entries: &[Peer]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let memory = Memory { peers: entries.to_vec() };
    let Ok(json) = serde_json::to_string(&memory) else { return String::new() };
    format!("{MARK}{} -->", json.replace("--", "-\\u002d"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_page(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("splash-page-{name}.html"));
        std::fs::remove_file(&path).ok();
        path
    }

    fn page_with(comment: &str) -> String {
        format!("<!DOCTYPE html>\n<html><body><h1>hi</h1>\n{comment}\n</body></html>\n")
    }

    fn peer(name: &str, url: &str) -> Peer {
        Peer { name: name.into(), url: url.into(), ..Peer::default() }
    }

    #[test]
    fn what_the_page_remembered_can_be_read_back() {
        let page = temp_page("roundtrip");
        let entries = merge_at(&[], &[peer("ginger", "http://ginger.local:6420")], Duration::from_secs(86400), 1_000);
        std::fs::write(&page, page_with(&comment(&entries))).unwrap();

        let read_back = read_at(&page, Duration::from_secs(86400), 1_100);
        assert_eq!(read_back.len(), 1);
        assert_eq!(read_back[0].name, "ginger");
        assert_eq!(read_back[0].last_seen, Some(1_000), "the age travels inside the page");
        assert_eq!(
            read_back[0].source.as_deref(),
            Some(SOURCE_CACHE),
            "read back, it is a hint and not a discovery"
        );
        std::fs::remove_file(&page).ok();
    }

    #[test]
    fn a_page_without_a_memory_is_simply_the_first_run() {
        let page = temp_page("none");
        assert!(read_at(&page, Duration::from_secs(86400), 1_000).is_empty(), "a missing page");

        std::fs::write(&page, page_with("<!-- nothing to see -->")).unwrap();
        assert!(read_at(&page, Duration::from_secs(86400), 1_000).is_empty(), "no marker");

        std::fs::write(&page, page_with("<!-- CACHE {not json} -->")).unwrap();
        assert!(
            read_at(&page, Duration::from_secs(86400), 1_000).is_empty(),
            "a broken memory is not fatal"
        );
        std::fs::remove_file(&page).ok();
    }

    #[test]
    fn an_unterminated_comment_is_ignored() {
        let page = temp_page("unterminated");
        std::fs::write(&page, page_with("<!-- CACHE {\"peers\":[]}")).unwrap();
        assert!(read_at(&page, Duration::from_secs(86400), 1_000).is_empty());
        std::fs::remove_file(&page).ok();
    }

    #[test]
    fn an_entry_not_seen_again_keeps_its_age() {
        let ginger = peer("ginger", "http://ginger.local:6420");
        let basil = peer("basil", "http://basil.local:6420");
        let first = merge_at(&[], &[ginger.clone(), basil], Duration::from_secs(86400), 1_000);
        let second = merge_at(&first, &[ginger], Duration::from_secs(86400), 5_000);

        let basil = second.iter().find(|p| p.name == "basil").expect("unseen entries survive");
        assert_eq!(basil.last_seen, Some(1_000), "an entry not seen again does not get younger");
        let ginger = second.iter().find(|p| p.name == "ginger").unwrap();
        assert_eq!(ginger.last_seen, Some(5_000), "a machine seen again is refreshed");
    }

    #[test]
    fn an_old_memory_eventually_expires() {
        let entries = merge_at(&[], &[peer("old", "http://old.local:6420")], Duration::from_secs(86400), 1_000);
        let page = temp_page("expiry");
        std::fs::write(&page, page_with(&comment(&entries))).unwrap();

        assert_eq!(read_at(&page, Duration::from_secs(86400), 10_000).len(), 1, "within the window");
        assert!(
            read_at(&page, Duration::from_secs(86400), 1_000 + 86_401).is_empty(),
            "past the window the address is forgotten"
        );
        std::fs::remove_file(&page).ok();
    }

    #[test]
    fn the_same_machine_seen_by_two_spellings_is_one_entry() {
        let by_name = Peer {
            addrs: vec!["192.168.1.72".into()],
            ..peer("ginger", "http://ginger.local:6420")
        };
        let by_ip = peer("ginger-ip", "http://192.168.1.72:6420");
        let entries = merge_at(&[], &[by_name, by_ip], Duration::from_secs(86400), 1_000);
        assert_eq!(entries.len(), 1, "one machine, one remembered entry: {entries:?}");
    }

    #[test]
    fn the_comment_survives_hostile_text() {
        // A description containing `-->` would otherwise end the comment and dump the
        // JSON into the page as visible text.
        let entry = Peer {
            desc: Some("closed -- until --> now".into()),
            ..peer("awkward", "http://awkward.local:6420")
        };
        let comment = comment(std::slice::from_ref(&entry));
        assert_eq!(comment.matches("-->").count(), 1, "exactly one terminator: {comment}");

        let page = temp_page("hostile");
        std::fs::write(&page, page_with(&comment)).unwrap();
        let read_back = read_at(&page, Duration::from_secs(86400), 1_000);
        assert_eq!(read_back.len(), 1);
        assert_eq!(
            read_back[0].desc,
            Some("closed -- until --> now".into()),
            "the text round-trips intact"
        );
        std::fs::remove_file(&page).ok();
    }

    #[test]
    fn nothing_to_remember_writes_no_comment() {
        assert_eq!(comment(&[]), "");
    }
}

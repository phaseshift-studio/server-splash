use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::fs;
use crate::peers::Peer;
use crate::wizard::{SplashService, WizardOutput};

/// The studio that builds this tool, linked from every generated page.
pub(crate) const PHASESIT_URL: &str = "https://phaseshift.studio";

/// Icons shipped with the binary, written into `<output>/icons/` on every run.
///
/// Embedded rather than read from disk so the binary is self-contained: the icon
/// survives being copied to another machine, which is how it gets deployed.
pub(crate) const BUNDLED_ICONS: &[(&str, &[u8])] = &[
    ("metatron.png", include_bytes!("assets/metatron.png")),
    ("metatron@2x.png", include_bytes!("assets/metatron@2x.png")),
];

/// Minimal base64, so icons can be embedded without pulling in a dependency.
pub(crate) fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b1 = *chunk.first().unwrap_or(&0) as u32;
        let b2 = *chunk.get(1).unwrap_or(&0) as u32;
        let b3 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b1 << 16) | (b2 << 8) | b3;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "application/octet-stream",
    }
}

/// Bundled asset bytes for a path like `icons/metatron.png`, preferring the 2x file
/// when embedding (one source that is sharp on every display beats a srcset pair).
fn bundled_bytes(path: &str, prefer_retina: bool) -> Option<(&'static str, &'static [u8])> {
    let file = path.rsplit('/').next()?;
    if prefer_retina {
        if let Some(stem) = file.strip_suffix(".png") {
            let retina = format!("{stem}@2x.png");
            if let Some(hit) = BUNDLED_ICONS.iter().find(|(n, _)| *n == retina) {
                return Some((hit.0, hit.1));
            }
        }
    }
    BUNDLED_ICONS.iter().find(|(n, _)| *n == file).map(|(n, b)| (*n, *b))
}

/// Where an image icon should actually load from.
///
/// Files are the default only when embedding is turned off; embedding is preferred
/// because a relative `icons/…` path only resolves when the page is served from the
/// directory it was written to. That is not a hypothetical: this deployment already
/// has a subresource the server refuses to serve (`/ollama-ui/css/style.css` → 403),
/// and an icon that fails to load renders as a grey box.
fn icon_src(icon: &str, icon_data: Option<&[u8]>, inline: bool) -> String {
    if icon.starts_with("http://") || icon.starts_with("https://") {
        return icon.to_string(); // a curated remote logo; nothing local to embed
    }
    if !inline {
        return icon.to_string();
    }
    if let Some(bytes) = icon_data {
        return format!("data:{};base64,{}", mime_for(icon), base64(bytes));
    }
    if let Some((name, bytes)) = bundled_bytes(icon, true) {
        return format!("data:{};base64,{}", mime_for(name), base64(bytes));
    }
    icon.to_string() // a file the user supplied that we have no bytes for
}

/// Is this icon an image file rather than an emoji?
///
/// Emoji never contain a slash or an image extension, so this is unambiguous.
pub(crate) fn is_image_icon(icon: &str) -> bool {
    let lower = icon.trim().to_lowercase();
    if !(lower.contains('/') || lower.starts_with("http")) {
        return false;
    }
    [".png", ".jpg", ".jpeg", ".gif", ".ico", ".svg", ".webp", ".bmp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Write via a temporary file and rename.
///
/// `--serve` reads this file while the refresh thread rewrites it, and the rename means
/// a reader sees either the old page or the new one, never half of either.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp-write");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

/// Write the bundled icons and any icons fetched from services.
fn write_icons(output: &WizardOutput) -> Result<(), String> {
    let dir = output.output_dir.join("icons");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create icons dir: {e}"))?;

    for (name, bytes) in BUNDLED_ICONS {
        std::fs::write(dir.join(name), bytes)
            .map_err(|e| format!("Failed to write icon {name}: {e}"))?;
    }

    for svc in &output.selected_services {
        if let Some(bytes) = &svc.icon_data {
            // The catalog names these `icons/<port>.<ext>`.
            if let Some(file) = svc.icon.rsplit('/').next() {
                std::fs::write(dir.join(file), bytes)
                    .map_err(|e| format!("Failed to write icon {file}: {e}"))?;
            }
        }
    }
    Ok(())
}

/// Escape text for use in HTML content.
fn esc(s: &str) -> Cow<'_, str> {
    if s.contains(['&', '<', '>', '"']) {
        Cow::Owned(
            s.replace('&', "&amp;")
             .replace('<', "&lt;")
             .replace('>', "&gt;")
             .replace('"', "&quot;"),
        )
    } else {
        Cow::Borrowed(s)
    }
}

/// Escape text for use in a double-quoted HTML attribute.
fn esc_attr(s: &str) -> String {
    esc(s).replace('\'', "&#39;")
}

/// Sort key for a group heading.
///
/// The original code keyed grouping off a `BTreeMap`, which silently reordered the
/// page alphabetically and put "Remote & Guest VMs" above "Host Services". Order is a
/// presentation decision, so it is made explicitly here.
fn group_rank(name: &str) -> (u8, String) {
    match name {
        "Host Services" => (0, String::new()),
        "Remote & Guest VMs" => (1, String::new()),
        "Monitoring" => (2, String::new()),
        "Development" => (3, String::new()),
        other => (4, other.to_lowercase()),
    }
}

fn group_icon(name: &str) -> &'static str {
    match name {
        "Host Services" => "&#x2699;&#xFE0F;",
        "Remote & Guest VMs" => "&#x1F3E1;&#xFE0F;",
        "Monitoring" => "&#x1F4C8;",
        "Development" => "&#x1F6E0;&#xFE0F;",
        "Storage & Sharing" => "&#x1F5C4;&#xFE0F;",
        _ => "&#x1F539;",
    }
}

/// Protocols a browser can actually follow from the dashboard.
fn is_linkable(protocol: &str) -> bool {
    matches!(protocol, "http" | "https" | "ssh" | "vnc" | "mqtt" | "ftp" | "ftps")
}

/// Full URL for a service, or `None` when it should not be a link.
///
/// A raw `tcp` service (SMB, a database, …) must not become a link: the old code
/// wrapped everything in `http://`, so clicking "MySQL" opened a broken page. Those
/// render as plain cards showing their protocol and port instead.
fn card_href(svc: &SplashService, hostname: &str) -> Option<String> {
    let port = svc.port.as_deref().unwrap_or("");
    if port.is_empty() || svc.local_only || !is_linkable(&svc.protocol.to_lowercase()) {
        return None;
    }
    let host = esc(svc.host_override.as_deref().unwrap_or(hostname));
    let bp = esc(svc.base_path.as_deref().unwrap_or("")).into_owned();
    Some(match svc.protocol.to_lowercase().as_str() {
        "ssh" => format!("ssh://{host}:{port}{bp}"),
        "vnc" => format!("vnc://{host}:{port}{bp}"),
        "mqtt" => format!("mqtt://{host}:{port}{bp}"),
        "https" => format!("https://{host}:{port}{bp}"),
        _ => format!("http://{host}:{port}{bp}"),
    })
}

/// The trailing chip on a card: `http: :11434`, or a lock for unreachable things.
fn render_chip(svc: &SplashService) -> String {
    let port = svc.port.as_deref().unwrap_or("");
    if port.is_empty() {
        return "<div class=\"link-wrap\"><span class=\"locked\">&#x1F512; daemon only</span></div>".to_string();
    }
    if svc.local_only {
        return "<div class=\"link-wrap\"><span class=\"locked\">&#x1F512; localhost only</span></div>".to_string();
    }
    let proto_lower = svc.protocol.to_lowercase();
    let proto = esc(&proto_lower);
    format!(
        "<div class=\"link-wrap\"><span class=\"protocol\">{proto}:</span><span class=\"link\">:{port}</span></div>"
    )
}

fn render_card(svc: &SplashService, hostname: &str, inline_icons: bool) -> String {
    let href = card_href(svc, hostname);
    let probe = svc
        .web_probe_url
        .as_deref()
        .filter(|u| !u.is_empty() && !u.eq_ignore_ascii_case("none"));

    // Initial state comes from the generation-time probe so the page is useful even
    // with JavaScript disabled; the script then refreshes it live.
    let initial = if probe.is_none() {
        "status-unknown"
    } else if svc.alive {
        "status-up"
    } else {
        "status-down"
    };

    let tag = if href.is_some() { "a" } else { "div" };
    let mut out = String::new();
    out.push_str(&format!(
        "<{tag} class=\"card {initial}{local}{unlinked}\"",
        local = if svc.local_only { " local-only" } else { "" },
        unlinked = if href.is_none() { " no-link" } else { "" },
    ));
    if let Some(h) = &href {
        out.push_str(&format!(" href=\"{}\"", esc_attr(h)));
    }
    if let Some(p) = probe {
        out.push_str(&format!(" data-probe=\"{}\"", esc_attr(p)));
    }
    out.push_str(&format!(" data-service=\"{}\">\n", esc_attr(&svc.name)));
    out.push_str("  <span class=\"status-icon\" aria-hidden=\"true\">&#x25CF;</span>\n");
    if is_image_icon(&svc.icon) {
        let src = esc_attr(&icon_src(&svc.icon, svc.icon_data.as_deref(), inline_icons));
        // When embedding we already chose the 2x bytes, so a srcset would be a
        // second copy of the same picture.
        let srcset = if inline_icons {
            String::new()
        } else {
            svc.icon
                .strip_suffix(".png")
                .map(|stem| format!("{stem}@2x.png"))
                .filter(|candidate| BUNDLED_ICONS.iter().any(|(n, _)| candidate.ends_with(n)))
                .map(|retina| format!(" srcset=\"{retina} 2x\""))
                .unwrap_or_default()
        };
        out.push_str(&format!(
            "  <span class=\"icon\"><img class=\"icon-img\" src=\"{src}\"{srcset} alt=\"\"></span>\n"
        ));
    } else {
        out.push_str(&format!("  <span class=\"icon\">{}</span>\n", esc_attr(&svc.icon)));
    }
    out.push_str(&format!(
        "  <div class=\"info\"><span class=\"name\">{}</span><br><span class=\"desc\">{}</span></div>\n",
        esc_attr(&svc.name),
        esc_attr(&svc.desc)
    ));
    out.push_str(&format!("  {}\n", render_chip(svc)));
    out.push_str(&format!("</{tag}>\n"));
    out
}

/// Render every group, in rank order, with services sorted by port for stable diffs.
fn render_groups(services: &[SplashService], hostname: &str, inline_icons: bool) -> String {
    let mut grouped: BTreeMap<String, Vec<&SplashService>> = BTreeMap::new();
    for svc in services {
        if svc.hidden {
            continue;
        }
        let key = if svc.group.trim().is_empty() { "Host Services".to_string() } else { svc.group.clone() };
        grouped.entry(key).or_default().push(svc);
    }

    let mut names: Vec<String> = grouped.keys().cloned().collect();
    names.sort_by_key(|n| group_rank(n));

    let mut out = String::new();
    for name in names {
        let mut svcs = grouped.remove(&name).unwrap_or_default();
        svcs.sort_by_key(|s| (s.port_num().unwrap_or(u16::MAX), s.name.to_lowercase()));

        out.push_str(&format!(
            "<h2>{} {}</h2>\n<div class=\"grid\">\n",
            group_icon(&name),
            esc(&name)
        ));
        for svc in svcs {
            out.push_str(&render_card(svc, hostname, inline_icons));
        }
        out.push_str("</div>\n\n");
    }
    out
}

/// One card per peer machine.
///
/// Deliberately built from the same `.card` markup as a service card: the status
/// light, the hover treatment and the health script come for free, and a machine you
/// can click looks exactly like a service you can click.
fn render_peer_card(peer: &Peer, inline_icons: bool) -> String {
    let icon = peer.icon_or_default();
    // Provenance beats a curated tag: a machine that arrived over federation must say so,
    // so it is never mistaken for one that is in `peers.toml`.
    let tag_text = peer
        .learned_from
        .as_ref()
        .map(|from| format!("via {from}"))
        .or_else(|| {
            // A remembered address is a guess, so the card says how old the guess is.
            peer.last_seen.map(last_seen_text)
        })
        .or_else(|| {
            peer.group
                .as_deref()
                .map(str::trim)
                .filter(|g| !g.is_empty())
                .map(str::to_string)
        })
        .or_else(|| match peer.source.as_deref() {
            Some(crate::peers::SOURCE_MDNS) => Some("discovered".to_string()),
            _ => None,
        });
    let tag = tag_text
        .map(|g| format!("<span class=\"peer-tag\">{}</span>", esc(&g)))
        .unwrap_or_default();

    // A configured or remembered machine was asked at generation time, so the static
    // page already knows whether it answered; anything else stays unknown until the
    // browser's probe runs.
    let initial = match peer.alive {
        Some(true) => "status-up",
        Some(false) => "status-down",
        None => "status-unknown",
    };

    let mut out = String::new();
    out.push_str(&format!(
        "<a class=\"card {initial} peer\" href=\"{}\"",
        esc_attr(&peer.url)
    ));
    if peer.probes() {
        // The browser asks the other machine whether it is up, the same way it checks
        // a service here. `no-cors` means a peer without CORS headers still answers
        // the only question being asked: is anything listening?
        out.push_str(&format!(" data-probe=\"{}\"", esc_attr(&peer.url)));
    }
    // Deliberately *not* `data-service`: the resync pass harvests ports from those
    // cards, and this port belongs to a different machine.
    out.push_str(&format!(" data-peer=\"{}\">\n", esc_attr(&peer.name)));
    out.push_str("  <span class=\"status-icon\" aria-hidden=\"true\">&#x25CF;</span>\n");
    if is_image_icon(&icon) {
        out.push_str(&format!(
            "  <span class=\"icon\"><img class=\"icon-img\" src=\"{}\" alt=\"\"></span>\n",
            esc_attr(&icon_src(&icon, None, inline_icons))
        ));
    } else {
        out.push_str(&format!("  <span class=\"icon\">{}</span>\n", esc_attr(&icon)));
    }
    out.push_str(&format!(
        "  <div class=\"info\"><span class=\"name\">{}</span><br><span class=\"desc\">{}</span></div>\n",
        esc_attr(&peer.name),
        esc_attr(&peer.desc_or_default())
    ));
    out.push_str(&format!(
        "  <div class=\"link-wrap\">{tag}<span class=\"link\">{}</span></div>\n",
        esc_attr(&peer.display_target())
    ));
    out.push_str("</a>\n");
    out
}

/// The `Machines` section, or nothing at all when no peers are configured.
///
/// Empty means the generated page is byte-for-byte what it was before this feature
/// existed, which keeps the diff honest for everyone not using it.
fn render_peers(output: &WizardOutput, inline_icons: bool) -> String {
    let mut visible: Vec<&Peer> = output.peers.iter().filter(|p| !p.is_hidden()).collect();
    if visible.is_empty() {
        return String::new();
    }
    // Stable order: the machines you wrote down first, then the ones that turned up by
    // themselves; within each, by tag then name. Adding a peer must not reshuffle the
    // ones already on the page.
    visible.sort_by_key(|p| {
        (
            p.source.as_deref() != Some(crate::peers::SOURCE_CONFIG),
            p.group.as_deref().unwrap_or("").trim().to_lowercase(),
            p.name.to_lowercase(),
        )
    });

    let mut out = String::from("<h2>&#x1F5A5;&#xFE0F; Machines</h2>\n<div class=\"grid\">\n");
    for peer in visible {
        out.push_str(&render_peer_card(peer, inline_icons));
    }
    out.push_str("</div>\n\n");
    out
}

/// The chip row under the title: RAM, storage, cores, link speed.
fn render_sys_chips(output: &WizardOutput) -> String {
    output
        .sys_chips
        .iter()
        .map(|chip| format!("  <span class=\"chip\">{}</span>\n", esc_attr(chip)))
        .collect()
}

/// Hand the browser everything it needs to re-discover services on its own:
/// which ports are already shown, which ones to try, and how to present a hit.
///
/// This is what makes the in-page "Resync" button possible with no backend — a static
/// page fetching a JSON file could only ever show what the last build contained.
fn resync_config(output: &WizardOutput, hostname: &str) -> String {
    let known: Vec<u16> = output
        .selected_services
        .iter()
        .filter(|s| !s.hidden)
        .filter_map(|s| s.port_num())
        .collect();

    let catalog = crate::catalog::Catalog::load(&output.overrides_path);
    let payload = serde_json::json!({
        "host": hostname,
        "knownPorts": known,
        "candidates": crate::catalog::RESYNC_CANDIDATE_PORTS,
        "catalog": catalog.port_index(),
        "speedtest": {
            "file": if output.speedtest_bytes > 0 { "speedtest.bin" } else { "" },
            "bytes": output.speedtest_bytes,
        },
    });

    // `<` is escaped so a value can never terminate the surrounding <script>.
    serde_json::to_string(&payload)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c")
}

/// Deterministic incompressible payload for the speed test.
///
/// Written only when it is missing or the wrong size, so a 15-minute timer does not
/// push 8 MB through the filesystem every quarter of an hour.
fn write_speedtest_payload(output: &WizardOutput) -> Result<(), String> {
    if output.speedtest_bytes == 0 {
        return Ok(());
    }
    let path = output.output_dir.join("speedtest.bin");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() == output.speedtest_bytes {
            return Ok(());
        }
    }

    let size = output.speedtest_bytes as usize;
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut bytes = Vec::with_capacity(size);
    while bytes.len() < size {
        // xorshift64*, so the payload does not compress and cannot be served from a
        // smaller cached copy.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        bytes.extend_from_slice(&state.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes());
    }
    bytes.truncate(size);

    std::fs::write(&path, bytes).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

/// The GPU panel: cards pre-filled from `nvidia-smi`, which the page then refreshes
/// from the Glances API when one is configured.
///
/// Without this the panel only appeared once a fetch resolved — and never at all if
/// Glances was absent or unreachable from the browser.
fn render_gpu_section(output: &WizardOutput) -> String {
    let live = output.glances_api_base.as_deref().unwrap_or("").trim();
    if output.gpus.is_empty() && live.is_empty() {
        return String::new();
    }

    let mut cards = String::new();
    for gpu in &output.gpus {
        let (colour, _) = if gpu.mem_pct > 80 {
            ("#ff6e6a", "high")
        } else if gpu.mem_pct > 40 {
            ("#e5f07a", "medium")
        } else {
            ("#9ece6a", "low")
        };
        let temp = gpu.temp_c.map(|t| format!("{t}\u{00B0}C")).unwrap_or_else(|| "-".into());
        let fan = gpu.fan_pct.map(|f| format!("{f}%")).unwrap_or_else(|| "\u{2014}".into());
        let util = gpu.util_pct.map(|u| format!("{u}%")).unwrap_or_else(|| "-".into());
        cards.push_str(&format!(
            r#"  <div class="gpu-card">
    <div class="gpu-header"><span class="gpu-icon">&#x1F4A1;</span> {name}</div>
    <strong style="color:{colour};font-size:1.55rem;">{mem}<span style="font-size:.85rem;color:#565f89;margin-left:.2rem">%</span></strong><span class="desc" style="color:#565f89;margin-left:.3rem;font-size:.78rem;">VRAM</span>
    <div class="gpu-metrics">
      <div class="gpu-metric"><span class="gpu-label">Compute:</span>{util}</div>
      <div class="gpu-metric"><span class="gpu-label">Temp:</span>{temp}</div>
      <div class="gpu-metric" style="grid-column: span 2;"><span class="gpu-label">Fan:</span>{fan}</div>
    </div>
  </div>
"#,
            name = esc_attr(&gpu.name),
            colour = colour,
            mem = gpu.mem_pct,
            util = esc_attr(&util),
            temp = esc_attr(&temp),
            fan = esc_attr(&fan),
        ));
    }

    format!(
        "<!-- GPU STATS -->\n<div class=\"gpu-section\" id=\"gpu-section\">\n  <div class=\"gpu-grid\" id=\"gpu-list\" data-live=\"{live}\">\n{cards}  </div>\n</div>\n"
    )
}

/// Green below 40% usage, amber above, red above 80% — the same scale the GPU
/// cards use, so a full CPU reads the same colour as a full VRAM.
fn usage_colour(pct: f64) -> &'static str {
    if pct > 80.0 {
        "#ff6e6a"
    } else if pct > 40.0 {
        "#e5f07a"
    } else {
        "#9ece6a"
    }
}

/// `62 GB` from a mebibyte count, in the binary units the chips already use.
fn fmt_memory(mb: u64) -> String {
    crate::discover::human_bytes(mb.saturating_mul(1024 * 1024))
}

/// The machine panel: CPU, load and memory sampled at generation time, refreshed
/// from the Glances API when one is configured.
///
/// This is the companion to the GPU panel: the same tile look and the same rule that
/// the page is useful before JavaScript runs. Glances only refreshes values in place,
/// so a failed poll leaves the build-time numbers rather than a blank tile.
fn render_system_section(output: &WizardOutput) -> String {
    let stats = &output.system;
    let live = output.glances_api_base.as_deref().unwrap_or("").trim();
    if stats.is_empty() && live.is_empty() {
        return String::new();
    }

    let cores = if stats.cpu_count > 0 { stats.cpu_count.to_string() } else { "-".to_string() };

    let cpu = stats.cpu_pct;
    let cpu_text = cpu.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let cpu_colour = cpu.map(|v| usage_colour(v as f64)).unwrap_or("#7aa2f7");
    let load_text = |v: Option<f32>| v.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".to_string());
    let (load1, load5, load15) = match stats.load {
        Some(l) => (load_text(Some(l.one)), load_text(Some(l.five)), load_text(Some(l.fifteen))),
        None => ("-".to_string(), "-".to_string(), "-".to_string()),
    };

    let mem_pct = stats.mem_pct();
    let mem_text = mem_pct.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
    let mem_colour = mem_pct.map(|v| usage_colour(v as f64)).unwrap_or("#7aa2f7");
    // Available is total minus used, so it would only repeat the tiles around it.
    let (mem_used, mem_swap) = match stats.mem {
        Some(m) => (
            format!("{} / {}", fmt_memory(m.used_mb), fmt_memory(m.total_mb)),
            if m.swap_total_mb == 0 {
                "none".to_string()
            } else {
                format!("{} / {}", fmt_memory(m.swap_used_mb), fmt_memory(m.swap_total_mb))
            },
        ),
        None => ("-".to_string(), "-".to_string()),
    };

    format!(
        r#"<!-- MACHINE STATS -->
<div class="sys-section">
  <div class="sys-grid" id="sys-list" data-live="{live}">
  <div class="sys-card" id="sys-cpu">
    <div class="sys-header"><span class="sys-icon">&#x1F5A5;</span> CPU</div>
    <strong class="sys-value"><span id="sys-cpu-pct" style="color:{cpu_colour};">{cpu_text}</span><span class="sys-unit">%</span></strong><span class="sys-note">usage</span>
    <div class="sys-metrics">
      <div class="sys-metric"><span class="sys-label">Cores:</span><span id="sys-cpu-cores">{cores}</span></div>
      <div class="sys-metric"><span class="sys-label">Load 1m:</span><span id="sys-load1">{load1}</span></div>
      <div class="sys-metric"><span class="sys-label">Load 5m:</span><span id="sys-load5">{load5}</span></div>
      <div class="sys-metric"><span class="sys-label">Load 15m:</span><span id="sys-load15">{load15}</span></div>
    </div>
  </div>
  <div class="sys-card" id="sys-mem">
    <div class="sys-header"><span class="sys-icon">&#x1F9E0;</span> Memory</div>
    <strong class="sys-value"><span id="sys-mem-pct" style="color:{mem_colour};">{mem_text}</span><span class="sys-unit">%</span></strong><span class="sys-note">used</span>
    <div class="sys-metrics">
      <div class="sys-metric" style="grid-column: span 2;"><span class="sys-label">Used:</span><span id="sys-mem-used">{mem_used}</span></div>
      <div class="sys-metric" style="grid-column: span 2;"><span class="sys-label">Swap:</span><span id="sys-mem-swap">{mem_swap}</span></div>
    </div>
  </div>
  </div>
</div>
"#,
    )
}

/// `2026-10-16 15:42` — no chrono dependency for one timestamp.
/// The shortest honest form of an age: `just now`, `12m ago`, `3h ago`, `6d ago`.
fn ago_text(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3_600..=86_399 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `last seen 3h ago` — the label on a machine remembered from an earlier run.
fn last_seen_text(epoch: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(epoch);
    format!("last seen {}", ago_text(now.saturating_sub(epoch)))
}

fn now_string() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

pub(crate) fn generate(output: &WizardOutput) -> Result<String, String> {
    let hostname = &output.hostname;
    let out_dir = output.output_dir.clone();
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output dir: {e}"))?;

    let mut html = include_str!("template.html").to_string();

    let title = if output.title.is_empty() { hostname.clone() } else { output.title.clone() };
    let subtitle = if output.subtitle.is_empty() {
        "Quick-launch dashboard — services directory".to_string()
    } else {
        output.subtitle.clone()
    };

    html = html.replace("<!-- TITLE -->", &esc(&title));
    html = html.replace("<!-- SUBTITLE -->", &esc(&subtitle));
    html = html.replace(
        "<!-- GROUPS -->",
        &render_groups(&output.selected_services, hostname, output.inline_icons),
    );
    html = html.replace("<!-- GPU_SECTION -->", &render_gpu_section(output));
    html = html.replace("<!-- SYSTEM_SECTION -->", &render_system_section(output));
    html = html.replace("<!-- SYS_CHIPS -->", &render_sys_chips(output));
    html = html.replace("<!-- PEERS -->", &render_peers(output, output.inline_icons));
    html = html.replace("<!-- SYSPLASH_CONFIG -->", &resync_config(output, hostname));
    html = html.replace("<!-- MODULES -->", &output.extra_cards.join(""));
    // The page remembers what it found, so the next generation can retry it.
    html = html.replace("<!-- CACHE -->", &crate::cache::comment(&output.cache));

    let ip = if output.ip_display.is_empty() { "*".to_string() } else { output.ip_display.clone() };
    let generated = now_string();
    let stamp = if generated.is_empty() { String::new() } else { format!(" — generated {generated}") };

    // The text parts are escaped, but this footer deliberately carries a real link, so
    // it is assembled rather than escaped as a whole.
    let footer_plain = format!(
        "{hostname} — {ip}{stamp} | powered by server-splash (build {}) — ",
        crate::build_stamp()
    );
    let footer_text = esc(&footer_plain);
    html = html.replace(
        "<!-- FOOTER -->",
        &format!(
            "{footer_text}<a href=\"{PHASESIT_URL}\" target=\"_blank\" rel=\"noopener noreferrer\">PhaseShift Studio</a>"
        ),
    );

    if !output.inline_icons {
        write_icons(output)?;
    }
    write_speedtest_payload(output)?;
    // The two files a peer reads: who we are, and who we vouch for.
    write_identity(output, &title)?;
    write_peers_file(output)?;

    let file_name = if output.output_file.is_empty() { "index.html".to_string() } else { output.output_file.clone() };
    let out_path = out_dir.join(&file_name);
    write_atomic(&out_path, html.as_bytes())
        .map_err(|e| format!("Failed to write {}: {e}", out_path.display()))?;

    // Machine-readable companion, useful for other tooling and for tests. It is
    // deliberately excluded from deploys (see main.rs) because it lists every port.
    write_dashboard_json(output, &title, &subtitle, &generated);

    Ok(out_path.to_string_lossy().to_string())
}

/// Publish this dashboard's identity at `/.well-known/splash.json`.
///
/// Identity only: what it is, what version, and where its peer list lives. It names no
/// service and no port, for the same reason `dashboard.json` is never deployed — a file
/// announcing the box should not also enumerate the ways in. Anything that scans for
/// dashboards can use this to tell one from an unrelated web server.
fn write_identity(output: &WizardOutput, title: &str) -> Result<(), String> {
    let dir = output.output_dir.join(".well-known");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;

    let doc = serde_json::json!({
        "generator": crate::peers::PeerDoc::GENERATOR,
        "version": env!("CARGO_PKG_VERSION"),
        "build": crate::build_stamp(),
        "name": output.hostname,
        "title": title,
        "peers": "peers.json",
    });
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    let path = dir.join("splash.json");
    std::fs::write(&path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

/// Publish `/peers.json`: the machines this dashboard will vouch for.
///
/// Only machines written into `peers.toml` are published (see `peers::advertised`) —
/// never one discovered over mDNS or learned from another dashboard. That is what keeps
/// gossip to one hop, and keeps this machine's LAN neighbours off the internet by
/// accident when the page is deployed to a public host.
fn write_peers_file(output: &WizardOutput) -> Result<(), String> {
    let path = output.output_dir.join("peers.json");

    // Turning publishing off must also retract what the previous run published.
    let advertised = if output.publish_peers {
        crate::peers::advertised(&output.peers)
    } else {
        Vec::new()
    };
    if advertised.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }

    let doc = crate::peers::PeerDoc::new(
        &output.hostname,
        env!("CARGO_PKG_VERSION"),
        advertised,
    );
    let text = doc.to_json().ok_or("could not serialise the peer list")?;
    std::fs::write(&path, text).map_err(|e| format!("Failed to write {}: {e}", path.display()))
}

fn write_dashboard_json(output: &WizardOutput, title: &str, subtitle: &str, generated: &str) {
    let services: Vec<serde_json::Value> = output
        .selected_services
        .iter()
        .filter(|s| !s.hidden)
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "name": s.name,
                "desc": s.desc,
                "icon": s.icon,
                "icon_is_image": is_image_icon(&s.icon),
                "protocol": s.protocol,
                "port": s.port,
                "group": s.group,
                "local_only": s.local_only,
                "source": s.source,
                "web_probe_url": s.web_probe_url,
                "alive_at_generation": s.alive,
            })
        })
        .collect();

    let system = &output.system;
    let load_average = system.load.map(|l| {
        serde_json::json!({ "one": l.one, "five": l.five, "fifteen": l.fifteen })
    });
    let memory = system.mem.map(|m| {
        serde_json::json!({
            "total_mb": m.total_mb,
            "used_mb": m.used_mb,
            "available_mb": m.available_mb,
            "swap_total_mb": m.swap_total_mb,
            "swap_used_mb": m.swap_used_mb,
        })
    });

    let doc = serde_json::json!({
        "generated_at": generated,
        "generator_build": crate::build_stamp(),
        "hostname": output.hostname,
        "ip": output.ip_display,
        "title": title,
        "subtitle": subtitle,
        "glances_api_base": output.glances_api_base,
        "cpu_pct": system.cpu_pct,
        "cpu_count": system.cpu_count,
        "load_average": load_average,
        "memory": memory,
        "services": services,
        "peers": output
            .peers
            .iter()
            .filter(|p| !p.is_hidden())
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "url": p.url,
                    "desc": p.desc,
                    "group": p.group,
                    "source": p.source,
                })
            })
            .collect::<Vec<_>>(),
    });

    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = fs::write(output.output_dir.join("dashboard.json"), text);
    }
}

/// Build an HTML link card for a dashboard module to inject into the splash page.
pub(crate) fn module_card(
    url_prefix: &str,
    icon: &str,
    name: &str,
    desc: &str,
    port: u16,
) -> String {
    format!(
        r#"<a class="card status-unknown" href="{url_prefix}/index.html" data-probe="none" data-service="{name}">
  <span class="status-icon" aria-hidden="true">&#x25CF;</span>
  <span class="icon">{icon}</span>
  <div class="info"><span class="name">{name}</span><br><span class="desc">{desc}</span></div>
  <div class="link-wrap"><span class="protocol">http:</span><span class="link">:{port}</span></div>
</a>
"#,
        url_prefix = esc_attr(url_prefix),
        icon = esc_attr(icon),
        name = esc_attr(name),
        desc = esc_attr(desc),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(name: &str, port: Option<&str>, group: &str) -> SplashService {
        SplashService {
            name: name.to_string(),
            desc: format!("{name} description"),
            icon: "🌐".to_string(),
            protocol: "http".to_string(),
            port: port.map(String::from),
            group: group.to_string(),
            web_probe_url: port.map(|p| format!("http://ginger.local:{p}/")),
            ..SplashService::blank()
        }
    }

    fn output_with(services: Vec<SplashService>) -> WizardOutput {
        WizardOutput {
            output_dir: std::env::temp_dir(),
            hostname: "ginger.local".to_string(),
            title: "ginger.local".to_string(),
            subtitle: "Dual RTX 3090 — Linux Mint 22.3 — services directory".to_string(),
            ip_display: "192.168.1.72".to_string(),
            selected_services: services,
            _probe_results: Vec::new(),
            selected_modules: Vec::new(),
            glances_api_base: Some("http://ginger.local:61208".to_string()),
            deploy_path: None,
            output_file: "index.html".to_string(),
            extra_cards: Vec::new(),
            gpus: Vec::new(),
            system: crate::discover::SystemStats::default(),
            sys_chips: Vec::new(),
            speedtest_bytes: 0,
            overrides_path: std::path::PathBuf::from("/nonexistent/overrides.toml"),
            inline_icons: true,
            peers: Vec::new(),
            publish_peers: true,
            cache: Vec::new(),
        }
    }

    #[test]
    fn host_services_render_before_remote_services() {
        let out = output_with(vec![
            svc("Hermes Web", Some("9119"), "Remote & Guest VMs"),
            svc("Ollama", Some("11434"), "Host Services"),
        ]);
        let html = render_groups(&out.selected_services, &out.hostname, true);
        let host = html.find("Host Services").expect("host heading");
        let remote = html.find("Remote &amp; Guest VMs").or_else(|| html.find("Remote & Guest VMs")).expect("remote heading");
        assert!(host < remote, "explicit ordering, not alphabetical: {html}");
    }

    #[test]
    fn local_only_services_are_not_links() {
        let mut s = svc("CUPS", Some("631"), "Host Services");
        s.local_only = true;
        let html = render_card(&s, "ginger.local", true);
        assert!(!html.contains("<a "), "a loopback-only service must not be clickable: {html}");
        assert!(html.contains("localhost only"));
    }

    #[test]
    fn non_http_services_are_not_http_links() {
        let mut sql = svc("MySQL", Some("3306"), "Host Services");
        sql.protocol = "tcp".to_string();
        sql.web_probe_url = None;
        let html = render_card(&sql, "ginger.local", true);
        assert!(html.starts_with("<div"), "a tcp service must not be an <a>: {html}");
        assert!(!html.contains("http://"), "no fake http link: {html}");
        assert!(html.contains("no-link"));
        assert!(html.contains("tcp:") && html.contains(":3306"), "port still shown: {html}");

        // ssh/vnc/mqtt remain real links, with the right scheme.
        let mut ssh = svc("SSH", Some("22"), "Host Services");
        ssh.protocol = "ssh".to_string();
        ssh.web_probe_url = None;
        let html = render_card(&ssh, "ginger.local", true);
        assert!(html.contains(r#"href="ssh://ginger.local:22""#), "{html}");
        assert!(!html.contains("data-probe"), "ssh cannot be health-checked from the browser: {html}");
    }

    #[test]
    fn daemon_only_services_render_as_divs() {
        let mut s = svc("Docker Engine", None, "Host Services");
        s.web_probe_url = None;
        let html = render_card(&s, "ginger.local", true);
        assert!(html.starts_with("<div"), "{html}");
        assert!(html.contains("daemon only"));
    }

    #[test]
    fn http_cards_carry_a_live_probe_url() {
        let s = svc("Ollama", Some("11434"), "Host Services");
        let html = render_card(&s, "ginger.local", true);
        assert!(html.contains(r#"href="http://ginger.local:11434""#), "{html}");
        assert!(html.contains(r#"data-probe="http://ginger.local:11434/""#), "{html}");
    }

    #[test]
    fn initial_status_reflects_the_generation_time_probe() {
        let mut up = svc("Ollama", Some("11434"), "Host Services");
        up.alive = true;
        assert!(render_card(&up, "h", true).contains("card status-up"));

        let mut down = svc("Ghost", Some("9999"), "Host Services");
        down.alive = false;
        assert!(render_card(&down, "h", true).contains("card status-down"));
    }

    #[test]
    fn footer_carries_the_studio_link_and_the_build_stamp() {
        let dir = std::env::temp_dir().join("splash-footer-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = output_with(vec![]);
        out.output_dir = dir.clone();
        let page = generate(&out).unwrap();
        let html = std::fs::read_to_string(&page).unwrap();

        let footer = html.split("<footer>").nth(1).unwrap().split("</footer>").next().unwrap();
        assert!(footer.contains(r#"<a href="https://phaseshift.studio""#), "{footer}");
        assert!(footer.contains(">PhaseShift Studio</a>"), "{footer}");
        assert!(footer.contains("powered by server-splash (build "), "{footer}");
        assert!(footer.contains("ginger.local — 192.168.1.72"), "{footer}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writes_are_atomic() {
        let dir = std::env::temp_dir().join("splash-atomic-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.html");

        write_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert!(!path.with_extension("tmp-write").exists(), "no debris left behind");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hidden_services_are_omitted() {
        let mut s = svc("Secret", Some("1234"), "Host Services");
        s.hidden = true;
        let html = render_groups(&[s], "ginger.local", true);
        assert!(!html.contains("Secret"), "{html}");
    }

    #[test]
    fn generated_page_has_no_placeholders_left() {
        let dir = std::env::temp_dir().join("splash-generator-test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = output_with(vec![svc("Ollama", Some("11434"), "Host Services")]);
        out.output_dir = dir.clone();

        let path = generate(&out).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        for placeholder in [
            "<!-- TITLE -->",
            "<!-- SUBTITLE -->",
            "<!-- GROUPS -->",
            "<!-- MODULES -->",
            "<!-- CACHE -->",
            "<!-- GPU_SECTION -->",
            "<!-- SYSTEM_SECTION -->",
            "<!-- FOOTER -->",
        ] {
            assert!(!html.contains(placeholder), "{placeholder} was not substituted");
        }
        assert!(html.contains("<title>ginger.local</title>"));
        assert!(html.contains("powered by server-splash"));
        assert!(html.contains("192.168.1.72"));
        // The machine-readable companion is written alongside.
        let meta = std::fs::read_to_string(dir.join("dashboard.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(parsed["services"][0]["name"], "Ollama");
        assert_eq!(parsed["services"][0]["port"], "11434");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Renders the *real* `ginger` inventory — the ports and titles the deployed page
    /// actually shows — so the output can be eyeballed against what is live. Also
    /// asserts the shape of the result, which is what protects against regressions.
    /// The preview lands in `target/preview/ginger.html`.
    #[test]
    fn renders_a_preview_of_the_real_ginger_inventory() {
        use crate::discover::{DiscoveredService, FetchedIcon, HttpInfo};

        /// How one live service presents itself to discovery.
        struct Live {
            port: u16,
            proc: Option<&'static str>,
            unit: Option<&'static str>,
            container: Option<&'static str>,
            image: Option<&'static str>,
            title: Option<&'static str>,
            favicon: Option<&'static str>,
        }
        let none = |port: u16| Live { port, proc: None, unit: None, container: None, image: None, title: None, favicon: None };

        let inventory: Vec<Live> = vec![
            Live { proc: Some("sshd"), ..none(22) },
            // Apache publishes a favicon, but a recognised service keeps its catalog icon.
            Live { proc: Some("apache2"), title: Some("Apache2 Ubuntu Default Page"), favicon: Some("ico"), ..none(80) },
            Live { proc: Some("mosquitto"), ..none(1883) },
            // Docker forwards every published port through a process of this name; it
            // must not become the service's identity.
            Live { container: Some("dockge"), image: Some("louislam/dockge:1"), proc: Some("docker-proxy"), favicon: Some("ico"), ..none(5001) },
            Live { container: Some("mariadb"), image: Some("mariadb:11.2"), proc: Some("docker-proxy"), ..none(3306) },
            Live { container: Some("chromadb"), proc: Some("docker-proxy"), image: Some("chromadb/chroma:1.5.3"), ..none(8025) },
            Live { proc: Some("something"), title: Some("Some Internal Tool"), favicon: Some("png"), ..none(8080) },
            Live { container: Some("ollama"), proc: Some("docker-proxy"), image: Some("ollama/ollama"), ..none(8082) },
            // metatron: recognised by container name, and again by page title on 8777.
            Live { container: Some("metatron"), proc: Some("docker-proxy"), ..none(8555) },
            Live { container: Some("weird-name"), image: Some("ghcr.io/phaseshift-studio/metatron:main"), title: Some("metatron — PhaseShift Studio"), ..none(8777) },
            Live { container: Some("prometheus"), proc: Some("docker-proxy"), ..none(9090) },
            Live { container: Some("all-smi"), proc: Some("docker-proxy"), ..none(9091) },
            // Above the kernel's ephemeral range (32768-60999) and previously filtered
            // out as if it were a throwaway test port.
            Live { container: Some("glances"), proc: Some("glances"), ..none(61208) },
            Live { container: Some("hindsight"), proc: Some("docker-proxy"), favicon: Some("png"), ..none(9999) },
            Live { container: Some("ollama"), proc: Some("docker-proxy"), image: Some("ollama/ollama"), ..none(11434) },
            Live { title: Some("Docker Application Container Engine"), ..none(11435) },
            Live { container: Some("nginx-proxy"), proc: Some("docker-proxy"), ..none(11437) },
            Live { container: Some("uvicorn-app"), proc: Some("docker-proxy"), ..none(11599) },
        ];

        let catalog = crate::catalog::Catalog::load(std::path::Path::new("/nonexistent/overrides.toml"));
        let services: Vec<SplashService> = inventory
            .iter()
            .filter_map(|l| {
                let mut d = DiscoveredService {
                    port: l.port,
                    remote_reachable: true,
                    proc_name: l.proc.map(String::from),
                    pid: None,
                    unit: l.unit.map(String::from),
                    unit_desc: None,
                    container: l.container.map(String::from),
                    image: l.image.map(String::from),
                    http: None,
                };
                if l.title.is_some() || l.favicon.is_some() || l.container.is_some() {
                    d.http = Some(HttpInfo {
                        scheme: "http".into(),
                        status: 200,
                        server: Some("nginx".into()),
                        title: l.title.map(String::from),
                        icon: l.favicon.map(|ext| FetchedIcon {
                            // Real bytes: a stub that is not a decodable image renders
                            // as a broken-image box in the preview.
                            bytes: include_bytes!("assets/metatron.png").to_vec(),
                            ext: ext.to_string(),
                            source: format!("http://127.0.0.1:{}/favicon.{}", l.port, ext),
                        }),
                    });
                }
                catalog.resolve(&d, "ginger.local", true)
            })
            .collect();

        assert_eq!(services.len(), inventory.len(), "every discovered port should resolve to a card");

        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/preview");
        std::fs::create_dir_all(&dir).unwrap();

        let out = WizardOutput {
            output_dir: dir,
            output_file: "ginger.html".into(),
            hostname: "ginger.local".into(),
            title: "ginger.local".into(),
            subtitle: "Dual RTX 3090 — Linux Mint 22.3 — services directory".into(),
            ip_display: "192.168.1.72".into(),
            selected_services: services,
            _probe_results: Vec::new(),
            selected_modules: Vec::new(),
            glances_api_base: Some("http://ginger.local:61208".into()),
            deploy_path: None,
            extra_cards: Vec::new(),
            gpus: vec![
                crate::discover::Gpu { name: "NVIDIA GeForce RTX 3090".into(), mem_pct: 59, temp_c: Some(53), fan_pct: Some(47), util_pct: Some(0) },
                crate::discover::Gpu { name: "NVIDIA GeForce RTX 3090".into(), mem_pct: 57, temp_c: Some(37), fan_pct: Some(0), util_pct: Some(12) },
                crate::discover::Gpu { name: "AMD GPU".into(), mem_pct: 0, temp_c: Some(46), fan_pct: None, util_pct: Some(0) },
            ],
            system: crate::discover::SystemStats {
                load: Some(crate::discover::LoadAverage { one: 2.83, five: 2.66, fifteen: 2.46 }),
                cpu_pct: Some(37),
                mem: Some(crate::discover::MemInfo {
                    total_mb: 62068,
                    used_mb: 23800,
                    available_mb: 38268,
                    swap_total_mb: 8192,
                    swap_used_mb: 0,
                }),
                cpu_count: 24,
            },
            sys_chips: vec![
                "64 GB RAM".into(),
                "12 TB across 3 disks".into(),
                "24 cores".into(),
                "eno1 1 Gb/s".into(),
            ],
            speedtest_bytes: 8 * 1024 * 1024,
            overrides_path: std::path::PathBuf::from("/nonexistent/overrides.toml"),
            inline_icons: true,
            peers: Vec::new(),
            publish_peers: true,
            cache: Vec::new(),
        };
        let path = generate(&out).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();

        // Everything should be identified, and nothing should look like a guess.
        // Match a rendered card, not the resync script's own fallback template.
        assert!(!html.contains(r#"data-service="Service on :"#), "nothing should fall through: {path}");

        // The service-agnostic globe should be rare now: only the genuinely unknown
        // port 8080 should still carry it.
        let globes = html.matches(">🌐<").count();
        assert!(globes <= 1, "only the unidentified service should still show a globe, found {globes}");

        // Spot-check the icons this iteration was about.
        assert!(html.contains("🪶"), "Apache should get the feather");
        // metatron's logo (both cards) and the unknown service's favicon are images;
        // everything else is an emoji.
        // Count rendered images, not the CSS class or the resync script's template.
        assert_eq!(
            html.matches(r#"src="data:image/png;base64,"#).count(),
            3,
            "two metatron cards + the 8080 favicon",
        );
        assert!(html.contains(r#"<img class="icon-img" src="data:image/png;base64,"#), "icons travel inside the page");
        assert!(!html.contains(r#"src="icons/"#), "no reference that could fail to resolve");
        assert!(html.contains("🎛️"), "all-smi should get its own icon");
        assert!(html.contains("🧠"), "Hindsight is recognised, so it keeps its catalog icon over its favicon");

        // Every container port is forwarded by docker-proxy; none may be named "Docker"
        // for that reason, and the tool actually listening must be named instead.
        assert!(html.contains(">Dockge<"), "Dockge must be named, not its Docker forwarding process");
        assert!(html.contains(">MariaDB<"), "nor may MariaDB be swallowed by Docker");
        // Glances listens on 61208, above the ephemeral range: it must reach the page.
        assert!(html.contains(">Glances<"), "Glances must survive the ephemeral filter");
        assert!(html.contains(r#"href="http://ginger.local:61208""#));

        // Liveness, GPUs and links.
        assert!(html.contains("card status-up"), "HTTP services were up when sampled");
        assert!(html.contains(r#"data-live="http://ginger.local:61208""#), "GPU cards poll Glances");
        assert!(html.contains("59"), "VRAM is baked in from nvidia-smi");
        assert!(
            html.contains(r#"id="sys-list" data-live="http://ginger.local:61208""#),
            "machine cards poll Glances"
        );
        assert!(html.contains(r#"id="sys-cpu-pct""#), "CPU usage is baked in from /proc");
        assert!(html.contains("2.83"), "load averages are baked in");
        assert!(html.contains(r#"<span id="sys-mem-pct""#), "memory usage is baked in");
        assert!(html.contains(r#"href="ssh://ginger.local:22""#));
        assert!(html.contains(r#"href="http://ginger.local:11434""#));
        assert!(html.contains("</html>"));
    }

    fn sample_system() -> crate::discover::SystemStats {
        crate::discover::SystemStats {
            load: Some(crate::discover::LoadAverage { one: 1.5, five: 1.25, fifteen: 0.75 }),
            cpu_pct: Some(23),
            mem: Some(crate::discover::MemInfo {
                total_mb: 16384,
                used_mb: 8192,
                available_mb: 8192,
                swap_total_mb: 0,
                swap_used_mb: 0,
            }),
            cpu_count: 8,
        }
    }

    #[test]
    fn machine_stats_render_without_javascript() {
        let mut out = output_with(vec![]);
        out.glances_api_base = None;
        out.system = sample_system();

        let html = render_system_section(&out);
        assert!(
            html.contains(r#"<span id="sys-cpu-pct" style="color:#9ece6a;">23</span>"#),
            "{html}"
        );
        assert!(
            html.contains("1.50") && html.contains("1.25") && html.contains("0.75"),
            "load averages: {html}"
        );
        assert!(html.contains(">8<"), "the core count qualifies the load: {html}");
        assert!(
            html.contains(r#"<span id="sys-mem-pct" style="color:#e5f07a;">50</span>"#),
            "{html}"
        );
        assert!(html.contains("8.0 GB / 16 GB"), "used of total: {html}");
        assert!(html.contains(">none<"), "a machine without swap says so, not 0 GB: {html}");
        assert!(html.contains("&#x1F5A5;") && html.contains("&#x1F9E0;"));

        // Nothing known and no Glances to fill it in: no panel, rather than a row of
        // dashes pretending to be data.
        let mut empty = output_with(vec![]);
        empty.glances_api_base = None;
        assert_eq!(render_system_section(&empty), "");

        // With Glances configured the cards render even before the first fetch, so the
        // browser has somewhere to put the live numbers.
        let live_only = output_with(vec![]);
        assert!(render_system_section(&live_only)
            .contains(r#"id="sys-list" data-live="http://ginger.local:61208""#));
    }

    #[test]
    fn dashboard_json_carries_the_machine_stats() {
        let dir = std::env::temp_dir().join("splash-system-json-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut out = output_with(vec![]);
        out.output_dir = dir.clone();
        out.system = sample_system();
        generate(&out).unwrap();

        let meta = std::fs::read_to_string(dir.join("dashboard.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(parsed["cpu_pct"], 23);
        assert_eq!(parsed["cpu_count"], 8);
        assert_eq!(parsed["load_average"]["one"], 1.5);
        assert_eq!(parsed["memory"]["total_mb"], 16384);
        assert_eq!(parsed["memory"]["used_mb"], 8192);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn system_chips_are_rendered_under_the_title() {
        let mut out = output_with(vec![]);
        out.sys_chips = vec!["64 GB RAM".into(), "4.0 TB across 3 disks".into(), "eno1 1 Gb/s".into()];
        let html = render_sys_chips(&out);
        assert_eq!(html.matches("class=\"chip\"").count(), 3);
        assert!(html.contains("64 GB RAM"));
        assert!(html.contains("4.0 TB across 3 disks"));

        // A machine we could learn nothing about renders no chips at all rather than
        // a row of placeholders.
        let empty = output_with(vec![]);
        assert_eq!(render_sys_chips(&empty), "");
    }

    #[test]
    fn the_page_carries_a_usable_resync_config() {
        let mut out = output_with(vec![
            svc("Ollama", Some("11434"), "Host Services"),
            svc("metatron", Some("8555"), "Host Services"),
        ]);
        out.speedtest_bytes = 4 * 1024 * 1024;
        let json = resync_config(&out, "ginger.local");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");

        assert_eq!(parsed["host"], "ginger.local");
        assert_eq!(parsed["speedtest"]["file"], "speedtest.bin");
        assert_eq!(parsed["speedtest"]["bytes"], 4194304);

        let known: Vec<u64> = parsed["knownPorts"].as_array().unwrap()
            .iter().map(|v| v.as_u64().unwrap()).collect();
        assert!(known.contains(&11434) && known.contains(&8555), "existing ports are not re-added: {known:?}");

        let candidates = parsed["candidates"].as_array().unwrap();
        assert!(candidates.len() > 80);

        // The catalog must let the browser name what it finds.
        let catalog = parsed["catalog"].as_array().unwrap();
        let ollama = catalog.iter().find(|e| e["port"] == 11434).unwrap();
        assert_eq!(ollama["name"], "Ollama");
        assert_eq!(ollama["icon"], "🦙");

        // A speed test is disabled by an explicit zero rather than a missing file.
        let mut off = output_with(vec![]);
        off.speedtest_bytes = 0;
        let parsed: serde_json::Value = serde_json::from_str(&resync_config(&off, "h")).unwrap();
        assert_eq!(parsed["speedtest"]["file"], "");

        // `<` can never appear raw, or a value could close the surrounding <script>.
        assert!(!json.contains('<'), "JSON embedded in HTML must escape <");
    }

    #[test]
    fn speedtest_payload_is_written_once_at_the_right_size() {
        let dir = std::env::temp_dir().join("splash-speedtest");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut out = output_with(vec![]);
        out.output_dir = dir.clone();
        out.speedtest_bytes = 64 * 1024;
        write_speedtest_payload(&out).unwrap();
        let path = dir.join("speedtest.bin");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 64 * 1024);

        // The payload must not be trivially compressible, or a proxy would serve a
        // small copy and the measurement would read far higher than the real link.
        let sample = std::fs::read(&path).unwrap();
        let distinct: std::collections::HashSet<u8> = sample.into_iter().collect();
        assert!(distinct.len() > 200, "payload should look random, saw {} distinct bytes", distinct.len());

        // Rewriting is skipped when the size already matches, so a 15-minute timer
        // does not push megabytes through the disk every quarter hour.
        std::fs::write(&path, vec![0u8; 64 * 1024]).unwrap();
        write_speedtest_payload(&out).unwrap();
        assert!(std::fs::read(&path).unwrap().iter().all(|b| *b == 0), "unchanged when the size matches");

        // Zero disables it entirely.
        let mut off = output_with(vec![]);
        off.output_dir = dir.clone();
        off.speedtest_bytes = 0;
        std::fs::remove_file(&path).unwrap();
        write_speedtest_payload(&off).unwrap();
        assert!(!path.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn distinguishes_image_icons_from_emoji() {
        assert!(is_image_icon("icons/metatron.png"));
        assert!(is_image_icon("icons/8080.ico"));
        assert!(is_image_icon("https://example.com/a.svg"));
        assert!(!is_image_icon("🦙"));
        assert!(!is_image_icon("🌐"));
        assert!(!is_image_icon(""));
    }

    /// The base64 encoder is hand-rolled, so it is pinned against known vectors.
    #[test]
    fn base64_matches_the_reference_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0x89, 0x50, 0x4E, 0x47]), "iVBORw==", "real PNG magic");
        assert_eq!(mime_for("a.png"), "image/png");
        assert_eq!(mime_for("a.JPG"), "image/jpeg");
        assert_eq!(mime_for("a.svg"), "image/svg+xml");
        assert_eq!(mime_for("a.unknown"), "application/octet-stream");
    }

    #[test]
    fn image_icons_are_embedded_by_default_and_linked_on_request() {
        let mut s = svc("metatron", Some("8555"), "Host Services");
        s.icon = "icons/metatron.png".to_string();

        // Embedded: the bytes travel with the page, so it renders wherever it is
        // served from — a preview, a subdirectory, file:// — and cannot 403.
        let embedded = render_card(&s, "ginger.local", true);
        assert!(embedded.contains(r#"<img class="icon-img" src="data:image/png;base64,"#), "{embedded}");
        assert!(!embedded.contains(r#"src="icons/"#), "no path that can fail to resolve: {embedded}");
        assert!(!embedded.contains("srcset"), "the 2x bytes are already the embedded source");

        // The 2x asset is preferred when embedding, for a sharp result on any display.
        let two_x = include_bytes!("assets/metatron@2x.png");
        assert!(embedded.contains(&base64(two_x)), "should embed the 2x asset");

        // Opting out restores file references with a retina srcset.
        let linked = render_card(&s, "ginger.local", false);
        assert!(linked.contains(r#"src="icons/metatron.png""#), "{linked}");
        assert!(linked.contains(r#"srcset="icons/metatron@2x.png 2x""#), "{linked}");

        let emoji = svc("Ollama", Some("11434"), "Host Services");
        assert!(render_card(&emoji, "h", true).contains("<span class=\"icon\">🌐</span>"));
    }

    #[test]
    fn fetched_icons_are_embedded_or_written_out() {
        let png = include_bytes!("assets/metatron.png").to_vec();

        let mut with_icon = svc("Chromadb", Some("8025"), "Host Services");
        with_icon.icon = "icons/8025.png".to_string();
        with_icon.icon_data = Some(png.clone());

        // Embedded (the default): no icons directory is needed at all.
        let dir = std::env::temp_dir().join("splash-icons-inline");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let mut out = output_with(vec![with_icon.clone()]);
        out.output_dir = dir.clone();
        let page = generate(&out).unwrap();
        let html = std::fs::read_to_string(&page).unwrap();
        assert!(html.contains(&base64(&png)), "the service's own icon is inside the page");
        assert!(!dir.join("icons").exists(), "no icons dir when embedding");

        // Opted out: files are written and referenced.
        let dir2 = std::env::temp_dir().join("splash-icons-files");
        std::fs::remove_dir_all(&dir2).ok();
        std::fs::create_dir_all(&dir2).unwrap();
        let mut out2 = output_with(vec![with_icon]);
        out2.output_dir = dir2.clone();
        out2.inline_icons = false;
        let page2 = generate(&out2).unwrap();
        let html2 = std::fs::read_to_string(&page2).unwrap();
        assert!(dir2.join("icons").join("metatron.png").exists(), "bundled icons ship with the files");
        assert!(dir2.join("icons").join("8025.png").exists(), "fetched icons are mirrored");
        assert!(html2.contains("icons/8025.png"), "{html2}");

        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_dir_all(&dir2).ok();
    }

    fn peer(name: &str, url: &str) -> Peer {
        Peer { name: name.to_string(), url: url.to_string(), ..Peer::default() }
    }

    #[test]
    fn peers_render_as_a_machines_section() {
        let mut out = output_with(Vec::new());
        out.peers = vec![
            Peer { group: Some("LAN".into()), desc: Some("GPU box".into()), ..peer("ginger", "http://ginger.local:6420") },
            Peer { group: Some("WAN".into()), ..peer("basil", "https://basil.example.com") },
        ];
        let html = render_peers(&out, true);

        assert!(html.contains("Machines"), "the section needs a heading: {html}");
        assert!(html.contains(r#"href="http://ginger.local:6420""#), "{html}");
        assert!(html.contains(r#"href="https://basil.example.com""#), "{html}");
        assert!(html.contains(">LAN<") && html.contains(">WAN<"), "the group tag is shown: {html}");
        assert!(html.contains("ginger.local:6420</span>"), "the chip drops the scheme: {html}");
        assert!(html.contains("GPU box"), "{html}");
        assert!(html.contains("basil.example.com</span>"), "a peer without a desc falls back to its host: {html}");

        // LAN sorts before WAN regardless of file order.
        assert!(html.find("ginger").unwrap() < html.find("basil").unwrap(), "{html}");
    }

    #[test]
    fn a_learned_peer_says_who_taught_us_about_it() {
        // Provenance outranks a curated tag: a machine you never wrote down must be
        // obviously not one of yours, even if the machine that told you about it had
        // tagged it.
        let mut out = output_with(Vec::new());
        out.peers = vec![Peer {
            group: Some("WAN".into()),
            learned_from: Some("bravo.local".into()),
            source: Some(crate::peers::SOURCE_FEDERATION.into()),
            ..peer("carol", "http://carol.local:6420")
        }];
        let html = render_peers(&out, true);
        assert!(html.contains("via bravo.local"), "the card names its source: {html}");
        assert!(!html.contains(">WAN<"), "and not the tag of the machine that mentioned it: {html}");
    }

    #[test]
    fn a_cached_peer_says_how_old_the_memory_is() {
        // The address is a hint, so the card must not present it as a discovery.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = output_with(Vec::new());
        out.peers = vec![Peer {
            source: Some(crate::peers::SOURCE_CACHE.into()),
            last_seen: Some(now - 3 * 3_600),
            ..peer("bravo", "http://bravo.local:6422")
        }];

        let html = render_peers(&out, true);
        assert!(html.contains("last seen 3h ago"), "the age is on the card: {html}");
        assert!(html.contains(r#"href="http://bravo.local:6422""#), "and it is still clickable: {html}");
    }

    #[test]
    fn the_page_carries_the_machines_it_should_remember() {
        let dir = std::env::temp_dir().join("splash-memory-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut out = output_with(Vec::new());
        out.output_dir = dir.clone();
        out.peers = vec![Peer {
            source: Some(crate::peers::SOURCE_MDNS.into()),
            ..peer("ginger", "http://ginger.local:6420")
        }];
        out.cache = vec![peer("ginger", "http://ginger.local:6420")];

        let path = generate(&out).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("<!-- CACHE {"), "the memory rides in the page: {path}");
        assert!(!html.contains("<!-- CACHE -->"), "the slot itself is gone: {path}");

        // And the next generation can read it straight back out of the page it built.
        let remembered = crate::cache::read(std::path::Path::new(&path), std::time::Duration::from_secs(86_400));
        assert_eq!(remembered.len(), 1);
        assert_eq!(remembered[0].url, "http://ginger.local:6420");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_page_with_nothing_to_remember_carries_no_comment() {
        let dir = std::env::temp_dir().join("splash-memory-empty-test");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let mut out = output_with(Vec::new());
        out.output_dir = dir.clone();
        let path = generate(&out).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(!html.contains("<!-- CACHE"), "an empty memory leaves no trace: {path}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_peer_we_asked_shows_whether_it_answered() {
        let mut out = output_with(Vec::new());
        out.peers = vec![
            Peer { alive: Some(true), source: Some(crate::peers::SOURCE_CONFIG.into()), ..peer("up", "http://up.local:6420") },
            Peer { alive: Some(false), source: Some(crate::peers::SOURCE_CONFIG.into()), ..peer("down", "http://down.local:6420") },
            Peer { source: Some(crate::peers::SOURCE_MDNS.into()), ..peer("fresh", "http://fresh.local:6420") },
        ];
        let html = render_peers(&out, true);

        assert!(html.contains(r#"class="card status-up peer" href="http://up.local:6420""#), "{html}");
        assert!(html.contains(r#"class="card status-down peer" href="http://down.local:6420""#), "{html}");
        assert!(
            html.contains(r#"class="card status-unknown peer" href="http://fresh.local:6420""#),
            "a machine nobody asked is left to the browser: {html}"
        );
    }

    #[test]
    fn ages_are_reported_in_the_shortest_honest_unit() {
        assert_eq!(ago_text(0), "just now");
        assert_eq!(ago_text(59), "just now");
        assert_eq!(ago_text(60), "1m ago");
        assert_eq!(ago_text(3_599), "59m ago");
        assert_eq!(ago_text(3_600), "1h ago");
        assert_eq!(ago_text(86_399), "23h ago");
        assert_eq!(ago_text(86_400), "1d ago");
        assert_eq!(ago_text(29 * 86_400), "29d ago");
    }

    #[test]
    fn no_peers_means_no_machines_section() {
        let out = output_with(Vec::new());
        assert_eq!(render_peers(&out, true), "", "an unconfigured page must not grow an empty section");
    }

    #[test]
    fn hidden_peers_stay_out_of_the_page() {
        let mut out = output_with(Vec::new());
        out.peers = vec![
            Peer { hidden: Some(true), ..peer("retired", "http://old.local:6420") },
            peer("ginger", "http://ginger.local:6420"),
        ];
        let html = render_peers(&out, true);
        assert!(!html.contains("retired"), "hidden peers are kept in the file, not the page: {html}");
        assert!(html.contains("ginger"));
    }

    #[test]
    fn peer_cards_do_not_feed_the_resync_port_scan() {
        // The resync pass harvests ports from `.card[data-service]`; a peer's port
        // belongs to another machine and must not suppress a local discovery.
        let mut out = output_with(Vec::new());
        out.peers = vec![peer("ginger", "http://ginger.local:8080")];
        let card = render_peers(&out, true);
        assert!(!card.contains("data-service="), "peer cards must not be service cards: {card}");
        assert!(card.contains("data-peer="), "they are marked as peers instead: {card}");
        assert!(card.contains(r#"data-probe="http://ginger.local:8080""#), "reachability is checked live: {card}");
    }

    #[test]
    fn a_peer_can_opt_out_of_the_status_light() {
        let mut out = output_with(Vec::new());
        out.peers = vec![Peer { probe: Some(false), ..peer("far", "https://far.example.com") }];
        let html = render_peers(&out, true);
        assert!(!html.contains("data-probe"), "probe = false removes the live check: {html}");
        assert!(html.contains("https://far.example.com"), "the link still works: {html}");
    }

    #[test]
    fn peer_text_is_escaped() {
        let mut out = output_with(Vec::new());
        out.peers = vec![Peer {
            desc: Some(r#""><script>alert(1)</script>"#.into()),
            ..peer("xss", "http://ginger.local:6420")
        }];
        let html = render_peers(&out, true);
        assert!(!html.contains("<script>"), "a peer description must not inject markup: {html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
    }

    #[test]
    fn the_machines_section_lands_in_the_generated_page() {
        let mut out = output_with(vec![svc("Ollama", Some("11434"), "Host Services")]);
        out.output_dir = std::env::temp_dir().join("splash-machines-test");
        std::fs::create_dir_all(&out.output_dir).unwrap();
        out.peers = vec![peer("ginger", "http://ginger.local:6420")];

        let path = generate(&out).unwrap();
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("Machines"), "the slot must be filled: {path}");
        assert!(html.contains(r#"href="http://ginger.local:6420""#), "{path}");
        assert!(!html.contains("<!-- PEERS -->"), "the slot itself must be gone: {path}");

        let json = std::fs::read_to_string(out.output_dir.join("dashboard.json")).unwrap();
        assert!(json.contains(r#""peers""#), "peers are machine-readable too: {json}");
        assert!(json.contains("ginger.local:6420"), "{json}");

        std::fs::remove_dir_all(&out.output_dir).ok();
    }

    #[test]
    fn no_dead_logs_modal_reference_remains() {
        // The original page shipped a modal that fetched /api/logs, which nothing ever
        // served. Regenerating a page that advertises a 404 endpoint is worse than
        // having no logs feature at all.
        let template = include_str!("template.html");
        assert!(!template.contains("/api/logs"), "the dead logs endpoint must stay gone");
        assert!(template.contains("data-probe"), "the live status probe must be wired up");
        assert!(!template.contains("openservice("), "the broken click handler must stay gone");
    }
}

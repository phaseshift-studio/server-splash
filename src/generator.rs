use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use crate::wizard::{SplashService, WizardOutput};

fn group_key(svc: &SplashService) -> Cow<'static, str> {
    let override_ = svc.host_override.as_deref().unwrap_or("");
    if !override_.is_empty() && override_ != "localhost" {
        return Cow::Borrowed("Remote & Guest VMs");
    }
    match svc.group.as_str() {
        "Monitoring" | "Development" | "Storage & Sharing" | "Infrastructure" => Cow::Owned(svc.group.clone()),
        _ => Cow::Borrowed("Host Services"),
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
    }
}

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

fn card_class(svc: &SplashService) -> &'static str {
    if svc.port.is_none() || svc.protocol.is_empty() {
        "status-down"
    } else {
        "status-up"
    }
}

fn render_right(svc: &SplashService) -> String {
    let port = svc.port.as_deref().unwrap_or("");
    if !port.is_empty() {
        let proto = capitalize(&svc.protocol).to_lowercase();
        format!("<span class='protocol'>{proto}:</span>")
    } else {
        let lock_text = String::from_utf8(vec![0xe2, 0x9a, 0xa2, 0x20, 0x64, 0x61, 0x65, 0x6d, 0x6f, 0x6e, 0x20, 0x6f, 0x6e, 0x6c, 0x79]).unwrap();
        format!("<span class='rd'>&#x1F512; {lock_text}</span>")
    }
}

fn card_href(svc: &SplashService, hostname: &str) -> String {
    let port = svc.port.as_deref().unwrap_or("");
    if port.is_empty() { return String::new(); }
    let host = esc(svc.host_override.as_deref().unwrap_or(hostname));
    let bp = esc(svc.base_path.as_deref().unwrap_or(""));
    match svc.protocol.to_lowercase().as_str() {
        "ssh" => format!("ssh://{}:{}{}", host, port, bp),
        "vnc" => format!("vnc://{}:{}{}", host, port, bp),
        _ => format!("http://{}:{}{}", host, port, bp),
    }
}

fn click_handler(svc: &SplashService) -> Option<String> {
    if svc.protocol.to_lowercase().as_str() == "ssh" {
        Some("logsmodal('SSH Server')".to_string())
    } else if svc.port.is_some() {
        let href = card_href(svc, "");
        Some(format!("openservice('{}')", esc(&href)))
    } else {
        None
    }
}

fn render_card(svc: &SplashService, hostname: &str) -> String {
    let status_cls = card_class(svc);
    let port_str = svc.port.as_deref().unwrap_or("");
    let icon = esc_short(&svc.icon);
    let name = esc_short(&svc.name);
    let desc = esc(&svc.desc).into_owned();

    let href_val = if port_str.is_empty() {
        String::new()
    } else {
        format!("href=\"{}\"", esc(&card_href(svc, hostname)))
    };

    let probe_attr = if let Some(ref url) = svc.web_probe_url {
        if !url.is_empty() && url != "none" {
            format!(r#"data-probe="{}""#, esc(url))
        } else { String::new() }
    } else if svc.port.is_some() {
        let h = esc(svc.host_override.as_deref().unwrap_or(hostname));
        let p = svc.port.as_deref().unwrap_or("");
        format!(r#"data-probe="http://{}:{}""#, esc(&h), esc(p))
    } else { String::new() };

    let right_html = render_right(svc);
    let onclick = click_handler(svc).map(|handler| format!("onclick=\"{}\"", handler));
    let card_tag = if !href_val.is_empty() { "a" } else { "div" };

    let mut c = String::new();
    c.push('<');
    c.push_str(card_tag);
    c.push_str(" class=\"card service-card ");
    c.push_str(status_cls);
    c.push('"');
    if !href_val.is_empty() { c.push(' '); c.push_str(&href_val); }
    if !probe_attr.is_empty() { c.push(' '); c.push_str(&probe_attr); }
    if let Some(ref onclick) = onclick { c.push(' '); c.push_str(onclick); }
    c.push('>');

    c.push_str("<span class='status-badge'></span>");
    c.push_str("<span class='icon'>");
    c.push_str(&icon);
    c.push_str("</span>");
    c.push_str("<div class='info'><span class='name'>");
    c.push_str(&name);
    c.push_str("</span><br><span class='desc'>");
    c.push_str(&desc);
    c.push_str("</span></div>");

    if !right_html.is_empty() {
        c.push_str("<div class='link-wrap'>");
        c.push_str(&right_html);
        c.push_str("</div>");
    }

    c.push(' ');
    c.push_str("</");
    c.push_str(card_tag);
    c.push('>');
    c
}

fn esc_short(s: &str) -> String {
    let truncated: String = s.chars().take(40).collect();
    esc(&truncated).into_owned()
}

/// For known groups (Host Services, Remote & Guest VMs) the template already
/// provides `<h2>` and `<div class="grid">`, so we just return the cards.
/// Custom groups get their own heading + grid wrapper.
fn format_group_section(name: &str, svcs: &[&SplashService], hostname: &str) -> String {
    let is_known = matches!(name, "Host Services" | "Remote & Guest VMs");

    let mut s = String::new();
    if !is_known {
        let disp: String = format!("[{}] {}", &name[0..=0].to_uppercase(), &name[1..]);
        s.push_str(&format!("<h2>{}</h2>\n<div class=\"grid\">\n", esc(&disp).into_owned()));
    }
    for svc in svcs {
        s.push_str(&render_card(svc, hostname));
    }
    if !is_known {
        s.push_str("</div>\n");
    }
    s
}

pub(crate) fn generate(output: &WizardOutput) -> Result<String, String> {
    let mut html = include_str!("template.html").to_string();

    // Step 1: Replace hostnames in all three spots
    let hn = esc(&output.hostname).into_owned();
    for _ in 0..3 {
        html = html.replace("<!-- HOSTNAME -->", &hn);
    }

    // Step 1.5: Replace system info placeholder with static machine card
    let machine = crate::machine::collect();
    let total_ram_gb = machine.total_memory_mb as f64 / 1024.0;
    let disk_pct = if machine.disk_total_gb > 0 {
        (machine.disk_used_gb as f64 / machine.disk_total_gb as f64 * 100.0) as u8
    } else { 0 };
    let system_info_html = format!(
        r#"<div class="gpu-card">
  <div class="gpu-header"><span class="gpu-icon">&#x1F4BB;</span> System</div>
  <div class="gpu-metrics">
    <div class="gpu-metric"><span class="gpu-label">{os}</span>{ram:.1} GB</div>
    <div class="gpu-metric"><span class="gpu-label">{cores} cores</span>{kernel}</div>
    <div class="gpu-metric" style="grid-column:span 2"><span class="gpu-label">Disk</span> {disk_used}/{disk_total} GB ({disk_pct}%)</div>
  </div>
</div>"#,
        os = machine.os.split_whitespace().next().unwrap_or("Linux"),
        kernel = machine.kernel,
        cores = machine.cpu_core_count,
        ram = total_ram_gb,
        disk_used = machine.disk_used_gb,
        disk_total = machine.disk_total_gb,
        disk_pct = disk_pct,
    );
    html = html.replace("<!-- SYSTEM_INFO -->", &system_info_html);

    // Step 1.6: Replace Glances API URL placeholder
    let glances_url = output.glances_api_base.as_deref().unwrap_or("http://localhost:61208");
    html = html.replace("<!-- GLANCES_URL -->", glances_url);

    // Step 2: Group services, preserving insertion order (use BTreeMap for deterministic ordering)
    let mut svc_grouped: BTreeMap<String, Vec<&SplashService>> = BTreeMap::new();
    for svc in &output.selected_services {
        let key = group_key(svc);
        svc_grouped.entry(key.into()).or_default().push(svc);
    }

    let hostname = &output.hostname;

    // Place known groups into template markers, unknown groups appended before footer
    let mut custom_sections = Vec::new();
    for (gname, svcs) in &svc_grouped {
        match gname.as_str() {
            "Host Services" => {
                if html.contains("<!--GROUP_HOST_SERVICES-->") {
                    html = html.replace("<!--GROUP_HOST_SERVICES-->", &format_group_section(gname, svcs, hostname));
                }
            }
            "Remote & Guest VMs" => {
                if html.contains("<!--GROUP_REMOTE_GUEST_VMS-->") {
                    html = html.replace("<!--GROUP_REMOTE_GUEST_VMS-->", &format_group_section(gname, svcs, hostname));
                }
            }
            _ => {
                custom_sections.push(format_group_section(gname, svcs, hostname));
            }
        }
    }

    // Insert custom sections before footer if any exist
    if !custom_sections.is_empty() {
        let combined: String = custom_sections.join("");
        html = html.replace("<!-- FOOTER -->", &format!("\n{}\n<!-- FOOTER -->", combined));
    }
    // Step 3: Fill title and IP from splash-meta.json
    let meta_path = output.output_dir.join("splash-meta.json");
    if let Ok(raw) = fs::read_to_string(&meta_path) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(t) = meta.get("title").and_then(|v| v.as_str()) {
                html = html.replace("<span id=\"page-title\"></span>", t);
                if let Some(ip) = meta.get("ip_display").and_then(|v| v.as_str()) {
                    let ip_span = format!("<span class='subtitle-ip'>[{ip}]</span>");
                    html = html.replace(
                        "<p class=\"subtitle\">Quick-launch dashboard for monitored services</p>",
                        &format!("Quick-launch dashboard for monitored services {ip_span}"),
                    );
                }
            }
        }
    }

    // Step 4: Footer
    let ip_display = fs::read_to_string(&meta_path).ok()
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
        .and_then(|m| m.get("ip_display").and_then(|x| x.as_str().map(String::from)))
        .unwrap_or_else(|| "*".to_string());

    let footer = format!("{} {} {} | powered by server-splash", output.hostname, "\u{2014}", ip_display);
    html = html.replace("<!-- FOOTER -->", &esc(&footer));

    // Step 5: Write to disk
    let out_dir = output.output_dir.clone();
    fs::create_dir_all(&out_dir).map_err(|e| format!("Failed to create output dir: {e}"))?;
    let out_path = out_dir.join("splash-server.html");
    fs::write(&out_path, &html).map_err(|e| format!("Failed to write file: {e}"))?;

    Ok(out_path.to_string_lossy().to_string())
}

/// Build an HTML link card for a dashboard module to inject into the splash page.
pub(crate) fn module_card(
    url_prefix: &str,
    icon: &str,
    name: &str,
    desc: &str,
    port: u16,
    service_port: u16
) -> String {
    format!(
        r#"<a class="card service-card status-up" href="{url_prefix}/index.html">
<span class='status-badge'></span>
<span class='icon'>{icon}</span>
<div class='info'><span class='name'>{name}</span><br><span class='desc'>{desc}</span></div>
<div class='link-wrap'><span class='protocol'>http:</span><span>{port}</span></div>
</a>"#
    )
}

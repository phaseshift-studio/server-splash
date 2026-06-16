use std::borrow::Cow;
use std::fs;
use crate::wizard::{SplashService, WizardOutput};

fn group_key(svc: &SplashService) -> &'static str {
    let override_ = svc.host_override.as_deref().unwrap_or("");
    if !override_.is_empty() && override_ != "localhost" {
        "Remote & Guest VMs"
    } else {
        "Host Services"
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
    if svc.protocol.is_empty() || svc.protocol.to_lowercase().starts_with("http") {
        "status-unknown"
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
        // daemon-only: use lock icon + text
        let s = String::from_utf8(vec![0xe2, 0x9a, 0xa2, 0x20, 0x64,
                                        0x61, 0x65, 0x6d, 0x6f, 0x6e,
                                        0x20, 0x6f, 0x6e, 0x6c, 0x79])
            .unwrap();
        format!("<span class='rd'>&#x1F512; {s}</span>")
    }
}

fn card_href(svc: &SplashService, hostname: &str) -> String {
    let port = svc.port.as_deref().unwrap_or("");
    if port.is_empty() { return String::new(); }
    let host = esc(&svc.host_override.as_deref().unwrap_or(hostname));
    let bp = esc(svc.base_path.as_deref().unwrap_or(""));
    match svc.protocol.to_lowercase().as_str() {
        "ssh" => format!("ssh://{}:{}{}", host, port, bp),
        "vnc" => format!("vnc://{}:{}{}", host, port, bp),
        _ => format!("http://{}:{}{}", host, port, bp),
    }
}

fn click_handler(svc: &SplashService) -> Option<String> {
    if svc.protocol.to_lowercase().as_str() == "ssh" {
        // Show logs modal for SSH cards
        Some(format!("logsmodal('SSH Server')"))
    } else if svc.port.is_some() {
        // Open service in new tab for HTTP/other probes
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
        let h = esc(&svc.host_override.as_deref().unwrap_or(hostname));
        let p = svc.port.as_ref().map(|x| x.as_str()).unwrap_or("");
        format!(r#"data-probe="http://{}:{}""#, esc(&h), esc(p))
    } else {
        String::new()
    };

    let right_html = render_right(svc);
    
    // Build onclick handler
    let onclick = click_handler(svc)
        .map(|handler| format!("onclick=\"{}\"", handler));

    // Use the correct tag (a or div)
    let tag = if href_val.is_empty() { "div" } else { "a" };

    // Build card with status-icon positioned absolute (matching live site pattern)
    let mut c = String::new();
    c.push('<');
    c.push_str(tag);
    c.push_str(" class=\"card ");
    c.push_str(status_cls);
    c.push_str("\"");
    if !href_val.is_empty() { c.push_str(" "); c.push_str(&href_val); }
    if !probe_attr.is_empty() { c.push_str(" "); c.push_str(&probe_attr); }
    if let Some(ref onclick) = onclick { c.push_str(" "); c.push_str(onclick); }
    c.push('>');

    // Status icon absolute top-right (&#128354; green circle for unknown)
    c.push_str("<span class='status-icon'>&#x1F7E2;</span>");

    // Icon
    c.push_str("<span class='icon'>");
    c.push_str(&icon);
    c.push_str("</span>");

    // Info block (name + description)
    c.push_str("<div class='info'><span class='name'>");
    c.push_str(&name);
    c.push_str("</span><br><span class='desc'>");
    c.push_str(&desc);
    c.push_str("</span></div>");

    // Right side (link-wrap with protocol)
    if onclick.is_some() {
        c.push_str("<div class='link-wrap'>");
        c.push_str(&right_html);
        c.push_str("</div>");
    } else {
        // Daemon-only cards: show the right_html directly
        c.push('<');
        c.push_str(tag);
        c.push('>');
        return c;
    }
    c.push_str(" ");

    let tag = if href_val.is_empty() { "div" } else { "a" };
    c.push_str("</");
    c.push_str(tag);
    c.push('>');

    c
}

fn esc_short(s: &str) -> String {
    let chars: Vec<char> = s.chars().take(40).collect();
    let truncated: String = chars.into_iter().collect();
    esc(&truncated).into_owned()
}

fn format_group_section(name: &str, svcs: &[&SplashService], hostname: &str) -> String {
    let mut s = String::new();

    match name {
        "Host Services" => {
            s.push_str("<h2>&#x2699;&#xFE0F; Host Services</h2>\n<div class=\"grid\">\n");
        }
        "Remote & Guest VMs" => {
            s.push_str("<h2>&#x1F3E1;&#xFE0F; Remote &amp; Guest VMs</h2>\n<div class=\"grid\">\n");
        }
        other => {
            let first = other.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('?');
            s.push_str("<h2>[");
            s.push_str(&first.to_string());
            s.push_str("] ");
            s.push_str(esc(other).as_ref());
            s.push_str("</h2>\n<div class=\"grid\">\n");
        }
    };

    for svc in svcs {
        s.push_str(&render_card(svc, hostname));
        s.push('\n');
    }

    s.push_str("</div>\n");
    s
}

pub(crate) fn generate(output: &WizardOutput) -> Result<String, String> {
    let mut html = include_str!("template.html").to_string();

    // Step 1: Replace hostname in all three spots (title tag, h1, span id=page-title)
    let hn = esc(&output.hostname).into_owned();
    for _ in 0..3 {
        html = html.replace("<!-- HOSTNAME -->", &hn);
    }

    // Step 2: Build groups and substitute into template markers
    let mut groups_by_type: Vec<(&'static str, Vec<&SplashService>)> = Vec::new();
    for svc in &output.selected_services {
        let key = group_key(svc);
        match groups_by_type.iter_mut().find(|(g, _)| *g == key) {
            Some((_, svcs)) => svcs.push(svc),
            None => groups_by_type.push((key, vec![svc])),
        }
    }

    let hostname = &output.hostname;

    for (idx, (gname, svcs)) in groups_by_type.iter().enumerate() {
        let section_html = format_group_section(gname, &svcs, hostname);
        if idx == 0 {
            html = html.replace("<!--GROUP_HOST_SERVICES-->", &section_html);
        } else if idx == 1 {
            html = html.replace("<!--GROUP_REMOTE_GUEST_VMS-->", &section_html);
        }
    }

    // Step 3: Fill title and IP from splash-meta.json
    let meta_path = output.output_dir.join("splash-meta.json");
    if let Ok(raw) = fs::read_to_string(&meta_path) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(t) = meta.get("title").and_then(|v| v.as_str()) {
                html = html.replace("<span id=\"page-title\"></span>", t);

                // Fill IP display if available
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

    // Step 4: Footer with hostname and IP
    let ip_display = if !meta_path.exists() || meta_path.is_file() {
        fs::read_to_string(&meta_path).ok()
            .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
            .and_then(|m| m.get("ip_display").and_then(|x| x.as_str().map(String::from)))
    } else {
        None
    }.unwrap_or_else(|| "*".to_string());

    let footer = format!("{} {} {} | powered by server-splash", output.hostname, "\u{2014}", ip_display);
    html = html.replace("<!-- FOOTER -->", &esc(&footer).into_owned());

    Ok(html)
}

//! The service catalog: turns a raw discovered endpoint into a presentable card.
//!
//! Resolution is deliberately deterministic. For a given discovered service, rules are
//! tried in order and the first one that matches *and* supplies a given field wins:
//!
//!   1. user rules from `overrides.toml` (your curation — always authoritative)
//!   2. built-in identity rules (process / container / unit name)
//!   3. built-in port rules (well-known homelab ports)
//!   4. evidence fallbacks (the page `<title>`, the systemd unit description…)
//!
//! Nothing here needs a network call, so regenerating the dashboard is fast and works
//! offline. An LLM may *suggest* text for entries that fall through to step 4, but its
//! suggestions are cached and never on the critical path.

use std::path::Path;

use crate::discover::DiscoveredService;
use crate::wizard::SplashService;

/// One curation rule. Every display field is optional so a rule can override just the
/// icon, or just the group, without restating the rest.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub(crate) struct Rule {
    /// Match on the listening port.
    #[serde(default)]
    pub port: Option<u16>,
    /// Match on a case-insensitive substring of the container/unit/process/image name.
    #[serde(rename = "match", default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub desc: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub base_path: Option<String>,
    #[serde(default)]
    pub web_probe_url: Option<String>,
    #[serde(default)]
    pub host_override: Option<String>,
    /// Keep the entry out of the generated page (it stays in `splash-state.json`).
    #[serde(default)]
    pub hidden: Option<bool>,
}

impl Rule {
    fn matches(&self, svc: &DiscoveredService) -> bool {
        if self.port.is_none() && self.matcher.is_none() {
            return false; // a rule with no criteria matches nothing
        }
        if let Some(p) = self.port {
            if p != svc.port {
                return false;
            }
        }
        if let Some(m) = &self.matcher {
            let needle = m.to_lowercase();
            // Build the evidence deliberately, because what looks like identity often
            // is not:
            //
            //  * The `Server:` header is excluded — almost everything here is served by
            //    nginx, so matching it would rename every unknown service "nginx".
            //  * `docker-proxy` is excluded when the container is known. Docker forwards
            //    every published port through a process of that name, so including it
            //    named Dockge, Portainer and everything else "Docker".
            //  * The page title is included: a service that calls itself
            //    "metatron — PhaseShift Studio" has identified itself, even when the
            //    container name says nothing.
            // A container-runtime forwarder is not evidence of what the service is;
            // see `is_runtime_plumbing`.
            let proc = svc.proc_name.as_deref().unwrap_or("");
            let proc = if crate::discover::is_runtime_plumbing(proc) { "" } else { proc };
            let haystack = format!(
                "{} {} {} {} {}",
                svc.identity(),
                proc,
                svc.unit.as_deref().unwrap_or(""),
                svc.image.as_deref().unwrap_or(""),
                svc.http.as_ref().and_then(|h| h.title.as_deref()).unwrap_or(""),
            )
            .to_lowercase();
            if !haystack.contains(&needle) {
                return false;
            }
        }
        true
    }
}

/// The `[[service]]` array of `overrides.toml`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct Overrides {
    #[serde(default)]
    service: Vec<Rule>,
}

/// A resolved catalog.
///
/// The rule sets are kept apart because they carry different amounts of certainty:
/// a user rule is a deliberate choice, an identity rule means we recognised the
/// service, and a port rule is only a guess from the port number. That distinction
/// decides whether the service's own favicon outranks it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Catalog {
    user: Vec<Rule>,
    identity: Vec<Rule>,
    port: Vec<Rule>,
}

impl Catalog {
    /// Every rule set in precedence order.
    fn all(&self) -> impl Iterator<Item = &Rule> {
        self.user.iter().chain(self.identity.iter()).chain(self.port.iter())
    }

    /// The rules that matched, most authoritative first.
    fn matches<'a>(&'a self, svc: &'a DiscoveredService) -> impl Iterator<Item = &'a Rule> {
        self.all().filter(move |r| r.matches(svc))
    }
}

impl Catalog {
    /// Load user rules from `path` (missing file is not an error — it just means no
    /// curation yet) and append the built-in table.
    pub(crate) fn load(path: &Path) -> Self {
        let mut rules: Vec<Rule> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            match toml::from_str::<Overrides>(&text) {
                Ok(o) => {
                    if !o.service.is_empty() {
                        eprintln!(
                            "  \x1b[36mcuration\x1b[0m: {} rule(s) from {}",
                            o.service.len(),
                            path.display()
                        );
                    }
                    rules.extend(o.service);
                }
                Err(e) => eprintln!(
                    "  \x1b[31mwarning\x1b[0m: {} is not valid TOML ({e}); ignoring it",
                    path.display()
                ),
            }
        }
        let (identity, port) = builtin_rules();
        Catalog { user: rules, identity, port }
    }

    /// Resolve one discovered service into a dashboard entry.
    ///
    /// Returns `None` when curation marks the service hidden.
    pub(crate) fn resolve(
        &self,
        svc: &DiscoveredService,
        hostname: &str,
        global_probe: bool,
    ) -> Option<SplashService> {
        let mut out = SplashService::blank();
        out.id = Some(svc.id());
        out.source = Some(svc.source());
        out.port = Some(svc.port.to_string());
        out.local_only = !svc.remote_reachable;
        out.alive = svc.http.is_some();

        // Scheme first: it decides which fallbacks make sense.
        let protocol = self
            .matches(svc)
            .find_map(|r| r.protocol.clone())
            .or_else(|| svc.http.as_ref().map(|h| h.scheme.clone()))
            .unwrap_or_else(|| "tcp".to_string())
            .to_lowercase();

        // Name: explicit rule, else the page title, else a prettified identity.
        // Remember which of those it was — only the last is a guess worth replacing.
        let rule_name = self.matches(svc).find_map(|r| r.name.clone());
        let title_name = svc
            .http
            .as_ref()
            .and_then(|h| h.title.clone())
            .filter(|t| is_useful_title(t));
        let (name, name_specific) = match rule_name {
            // A recognised service, or your curation: the project's own spelling.
            Some(name) => (name, true),
            // The service describing itself in its own markup is just as authoritative.
            None => match title_name {
                Some(title) => (title, true),
                None => (default_name(svc, &protocol), false),
            },
        };

        let rule_desc = self.matches(svc).find_map(|r| r.desc.clone());
        let (desc, desc_specific) = match rule_desc {
            Some(desc) => (desc, true),
            None => (default_desc(svc), false),
        };

        // ── icon ladder ──────────────────────────────────────────────────────
        // Ordered by how much we actually know:
        //   1. your curation                     — a deliberate choice
        //   2. an identity rule                  — we recognised the service
        //   3. the service's own favicon         — brand-accurate, and the only thing
        //                                          that rescues services we do not know
        //   4. a port rule                       — a guess from the port number alone
        //   5. the per-protocol default          — the "wireframe globe"
        let user_icon = self.user.iter().filter(|r| r.matches(svc)).find_map(|r| r.icon.clone());
        let identity_icon = self.identity.iter().filter(|r| r.matches(svc)).find_map(|r| r.icon.clone());
        let port_icon = self.port.iter().filter(|r| r.matches(svc)).find_map(|r| r.icon.clone());
        let favicon = svc.http.as_ref().and_then(|h| h.icon.as_ref());

        let (icon, icon_specific, icon_data) = if let Some(icon) = user_icon {
            (icon, true, None)
        } else if let Some(icon) = identity_icon {
            (icon, true, None)
        } else if let Some(fetched) = favicon {
            (
                format!("icons/{}.{}", svc.port, fetched.ext),
                true,
                Some(fetched.bytes.clone()),
            )
        } else if let Some(icon) = port_icon {
            (icon, true, None)
        } else {
            (default_icon(&protocol).to_string(), false, None)
        };

        let group = self
            .matches(svc)
            .find_map(|r| r.group.clone())
            .unwrap_or_else(|| "Host Services".to_string());

        let base_path = self.matches(svc).find_map(|r| r.base_path.clone());
        let host_override = self.matches(svc).find_map(|r| r.host_override.clone());
        let hidden = self.matches(svc).find_map(|r| r.hidden).unwrap_or(false);

        // Only http(s) can be health-checked from the browser.
        let probe_url = if protocol == "http" || protocol == "https" {
            self.matches(svc)
                .find_map(|r| r.web_probe_url.clone())
                .or_else(|| {
                    if !global_probe {
                        return None;
                    }
                    let host = host_override.as_deref().unwrap_or(hostname);
                    let path = base_path.as_deref().unwrap_or("");
                    Some(format!("{}://{}:{}{}", protocol, host, svc.port, path))
                })
        } else {
            None
        };

        out.name = name;
        out.desc = desc;
        out.icon = icon;
        out.icon_specific = icon_specific;
        out.name_specific = name_specific;
        out.desc_specific = desc_specific;
        out.icon_data = icon_data;
        out.protocol = protocol;
        out.group = group;
        out.base_path = base_path;
        out.host_override = host_override;
        out.hidden = hidden;
        out.web_probe_url = probe_url;

        if hidden {
            return None;
        }
        Some(out)
    }
}

/// A port→presentation mapping serialised into the page, so the browser can name a
/// service it discovers on resync without asking the server to be rebuilt.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub(crate) struct PortEntry {
    pub port: u16,
    pub name: String,
    pub desc: String,
    pub icon: String,
    pub protocol: String,
    pub group: String,
}

/// Ports the in-page "Resync" probe will try, beyond those the catalog already knows.
///
/// A browser cannot enumerate a remote host's listening sockets — there is no API for
/// it — so resync can only find services on ports somebody thought to look at. This
/// list is what makes that useful in practice for a homelab.
pub(crate) const RESYNC_CANDIDATE_PORTS: &[u16] = &[
    22, 25, 53, 80, 110, 111, 137, 138, 139, 143, 161, 389, 443, 445, 465, 514, 548, 587,
    631, 636, 993, 995, 1080, 1433, 1521, 1880, 1883, 2049, 2082, 2086, 2181, 2222, 2375,
    2376, 3000, 3001, 3128, 3306, 3389, 4000, 4243, 4369, 4444, 4505, 4506, 5000, 5001,
    5005, 5044, 5060, 5222, 5269, 5353, 5432, 5555, 5601, 5672, 5678, 5683, 5900, 5901,
    5984, 6000, 6080, 6120, 61613, 6379, 6443, 6667, 7000, 7077, 7474, 7687, 8000, 8001,
    8008, 8025, 8043, 8069, 8080, 8081, 8082, 8086, 8088, 8090, 8096, 8123, 8161, 8200,
    8222, 8333, 8443, 8500, 8529, 8554, 8555, 8686, 8765, 8777, 8834, 8880, 8888, 8920,
    8983, 9000, 9001, 9042, 9090, 9091, 9092, 9093, 9100, 9119, 9122, 9160, 9200, 9300,
    9418, 9443, 9600, 9981, 9999, 10000, 10250, 11211, 11434, 11599, 15672, 18080, 19132,
    22222, 25565, 27017, 28017, 32400, 32768, 50000, 51000, 51820, 61208,
];

impl Catalog {
    /// Port→presentation entries for the browser: user rules first, so curation wins.
    pub(crate) fn port_index(&self) -> Vec<PortEntry> {
        let mut index: Vec<PortEntry> = Vec::new();
        let mut seen: std::collections::HashSet<u16> = std::collections::HashSet::new();

        for rule in self.user.iter().chain(self.port.iter()).chain(self.identity.iter()) {
            let Some(port) = rule.port else { continue };
            if !seen.insert(port) {
                continue; // an earlier, stronger rule already claimed this port
            }
            let Some(name) = rule.name.clone() else { continue };
            index.push(PortEntry {
                port,
                name,
                desc: rule.desc.clone().unwrap_or_default(),
                icon: rule.icon.clone().unwrap_or_default(),
                protocol: rule.protocol.clone().unwrap_or_else(|| "http".to_string()),
                group: rule.group.clone().unwrap_or_else(|| "Host Services".to_string()),
            });
        }
        index.sort_by_key(|e| e.port);
        index
    }
}

/// Is this `<title>` worth showing as a service name?
fn is_useful_title(t: &str) -> bool {
    let low = t.trim().to_lowercase();
    !low.is_empty()
        && low != "index of /"
        && low != "apache2 ubuntu default page"
        && low != "apache2 debian default page"
        && low != "welcome to nginx!"
        && low != "it works!"
        && !low.starts_with("404")
        && !low.starts_with("403")
        && low.len() <= 60
}

/// `zigbee2mqtt` -> `Zigbee2mqtt`; `port-9090` is handled by the caller.
fn prettify(identity: &str) -> String {
    let cleaned = identity.replace(['_', '-', '.'], " ");
    let cleaned = cleaned.trim();
    let mut chars = cleaned.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
    }
}

fn default_name(svc: &DiscoveredService, protocol: &str) -> String {
    if svc.container.is_none() && svc.unit.is_none() && svc.proc_name.is_none() {
        return format!("Service on :{}", svc.port);
    }
    let base = prettify(&svc.identity());
    if base.is_empty() {
        return format!("{} on :{}", protocol.to_uppercase(), svc.port);
    }
    base
}

fn default_desc(svc: &DiscoveredService) -> String {
    if let Some(desc) = &svc.unit_desc {
        if !desc.is_empty() {
            return desc.clone();
        }
    }
    if let Some(image) = &svc.image {
        if !image.is_empty() {
            return format!("Docker image {image}");
        }
    }
    if let Some(p) = &svc.proc_name {
        return format!("Listening process: {p}");
    }
    format!("Listening on TCP port {}", svc.port)
}

fn default_icon(protocol: &str) -> &'static str {
    match protocol {
        "http" | "https" => "🌐",
        "ssh" => "🔐",
        "vnc" => "🖥️",
        "mqtt" => "📨",
        _ => "🔌",
    }
}

/// Built-in knowledge, split by how much it actually tells us.
///
/// `identity` rules match a container / systemd unit / process / page title, i.e. we
/// recognised the software. `port` rules only guess from the port number, so they are
/// weaker than a service's own favicon.
fn builtin_rules() -> (Vec<Rule>, Vec<Rule>) {
    let mut identity: Vec<Rule> = Vec::new();

    let mut by_match = |matcher: &str, name: &str, desc: &str, icon: &str| {
        identity.push(Rule {
            matcher: Some(matcher.to_string()),
            name: Some(name.to_string()),
            desc: Some(desc.to_string()),
            icon: Some(icon.to_string()),
            ..Rule::default()
        });
    };

    // ── PhaseShift Studio's own services ──
    by_match("metatron", "metatron", "PhaseShift Studio metatron VM", "icons/metatron.png");

    // ── LLM / AI ──
    by_match("ollama", "Ollama", "Local LLM inference server", "🦙");
    by_match("open-webui", "Open WebUI", "Chat interface for local AI models", "💬");
    by_match("openwebui", "Open WebUI", "Chat interface for local AI models", "💬");
    by_match("chroma", "ChromaDB", "Vector database for embeddings", "🧮");
    by_match("hindsight", "Hindsight", "Agent memory and knowledge base", "🧠");
    by_match("all-smi", "all-smi", "GPU / NPU metrics exporter", "🎛️");
    by_match("uvicorn", "Uvicorn", "Python ASGI application server", "⚡");
    by_match("vllm", "vLLM", "High-throughput LLM serving", "🚀");
    by_match("text-generation", "Text Generation WebUI", "Local LLM chat interface", "💬");
    by_match("whisper", "Whisper", "Speech-to-text service", "🎙️");

    // ── web servers and proxies ──
    by_match("apache", "Apache2", "Apache HTTP Server", "🪶");
    by_match("nginx", "nginx", "Web server and reverse proxy", "🔀");
    by_match("traefik", "Traefik", "Reverse proxy and load balancer", "🛰️");
    by_match("caddy", "Caddy", "Web server with automatic HTTPS", "🛰️");
    by_match("cloudflared", "Cloudflare Tunnel", "Secure outbound tunnel daemon", "⚙️");
    // Deliberately no rule for `docker-proxy`: it is Docker's port forwarder, not a
    // service. Matching it labelled every published container port "Docker".
    by_match("docker-socket-proxy", "Docker Socket Proxy", "Restricted Docker API access", "🐳");
    by_match("docker-compose-ui", "Docker Compose UI", "Compose stack manager", "🐳");
    by_match("watchtower", "Watchtower", "Automatic container updates", "🗼");
    by_match("cadvisor", "cAdvisor", "Container resource metrics", "📊");
    by_match("lazydocker", "Lazydocker", "Terminal Docker UI", "🐳");
    by_match("yacht", "Yacht", "Container management UI", "🖥️");
    by_match("docker", "Docker", "Docker Application Container Engine", "🐳");

    // ── containers / orchestration ──
    by_match("portainer", "Portainer", "Container management UI", "🖥️");
    by_match("dockge", "Dockge", "Docker Compose stack manager", "🐳");
    by_match("kubernetes", "Kubernetes", "Container orchestration", "☸️");
    by_match("k3s", "K3s", "Lightweight Kubernetes", "☸️");

    // ── monitoring ──
    by_match("glances", "Glances", "Real-time system monitoring", "📈");
    by_match("grafana", "Grafana", "Metrics and dashboard visualisation", "📊");
    by_match("prometheus", "Prometheus", "Metrics collection and alerting", "📈");
    by_match("netdata", "Netdata", "Real-time performance monitoring", "📈");
    by_match("uptime-kuma", "Uptime Kuma", "Service uptime monitoring", "⏱️");
    by_match("lact", "LACT GPU Control", "NVIDIA GPU monitoring and control", "🎮");
    by_match("nvidia", "NVIDIA", "GPU tooling", "🎮");

    // ── home automation / IoT ──
    by_match("zigbee2mqtt", "Zigbee2MQTT", "Home automation Zigbee bridge", "🏠");
    by_match("mosquitto", "Mosquitto MQTT", "Lightweight IoT messaging broker", "📨");
    by_match("homeassistant", "Home Assistant", "Home automation hub", "🏠");
    by_match("home-assistant", "Home Assistant", "Home automation hub", "🏠");
    by_match("esphome", "ESPHome", "ESP device firmware builder", "🔌");
    by_match("node-red", "Node-RED", "Flow-based automation editor", "🔀");

    // ── databases ──
    by_match("postgres", "PostgreSQL", "SQL database server", "🐘");
    by_match("mysql", "MySQL", "SQL database server", "🐬");
    by_match("mariadb", "MariaDB", "SQL database server", "🦭");
    by_match("mongodb", "MongoDB", "Document database", "🍃");
    by_match("mongo", "MongoDB", "Document database", "🍃");
    by_match("redis", "Redis", "In-memory key/value store", "🧱");
    by_match("minio", "MinIO", "S3-compatible object storage", "🪣");
    by_match("influxdb", "InfluxDB", "Time-series database", "📉");
    by_match("sqlite", "SQLite", "Embedded SQL database", "🪶");

    // ── media ──
    by_match("jellyfin", "Jellyfin", "Media server", "🎬");
    by_match("plex", "Plex", "Media server", "▶️");
    by_match("immich", "Immich", "Photo and video backup", "📷");
    by_match("photoprism", "PhotoPrism", "Photo library", "📷");
    by_match("navidrome", "Navidrome", "Music streaming server", "🎵");
    by_match("*arr", "Arr Suite", "Media automation", "🎬");

    // ── files / productivity ──
    by_match("nextcloud", "Nextcloud", "File sync and collaboration", "☁️");
    by_match("syncthing", "Syncthing", "Peer-to-peer file sync", "🔄");
    by_match("samba", "Samba", "Windows/SMB file sharing", "📁");
    by_match("smbd", "Samba", "Windows/SMB file sharing", "📁");
    by_match("transmission", "Transmission", "BitTorrent client", "⬇️");
    by_match("paperless", "Paperless-ngx", "Document archive", "📄");
    by_match("vaultwarden", "Vaultwarden", "Password manager", "🗝️");
    by_match("gitea", "Gitea", "Self-hosted Git service", "🍵");
    by_match("gitlab", "GitLab", "Git and CI platform", "🦊");
    by_match("jenkins", "Jenkins", "CI automation server", "🤵");

    // ── networking / security ──
    by_match("adguard", "AdGuard Home", "DNS filtering", "🛡️");
    by_match("pihole", "Pi-hole", "DNS sinkhole", "🛡️");
    by_match("wireguard", "WireGuard", "VPN tunnel", "🔒");
    by_match("openvpn", "OpenVPN", "VPN server", "🔒");
    by_match("cups", "CUPS", "Print server", "🖨️");
    by_match("fail2ban", "Fail2ban", "Intrusion prevention", "🚫");

    // ── terminals / dev ──
    by_match("code-server", "code-server", "Browser VS Code", "💻");
    by_match("ttyd", "Web Terminal", "Browser-based shell", "💻");
    by_match("wetty", "Web Terminal", "Browser-based shell", "💻");
    by_match("shellinabox", "Web Terminal", "Browser-based shell", "💻");
    by_match("jupyter", "Jupyter", "Interactive notebooks", "📓");
    by_match("hermes", "Hermes Web", "Agent web UI", "🤖");
    by_match("piper", "Piper TTS", "Local text-to-speech gateway", "🔊");
    by_match("openssh", "SSH Server", "Secure remote shell access", "🔐");
    by_match("sshd", "SSH Server", "Secure remote shell access", "🔐");

    // ── port guesses: weaker than a favicon, so they are consulted after it ──
    let mut port_rules: Vec<Rule> = Vec::new();
    let mut by_port = |port: u16, name: &str, desc: &str, icon: &str, proto: &str, group: &str| {
        port_rules.push(Rule {
            port: Some(port),
            name: Some(name.to_string()),
            desc: Some(desc.to_string()),
            icon: Some(icon.to_string()),
            protocol: Some(proto.to_string()),
            group: Some(group.to_string()),
            ..Rule::default()
        });
    };

    by_port(22, "SSH Server", "Secure remote shell access", "🔐", "ssh", "Host Services");
    by_port(80, "HTTP Server", "HTTP web root", "🌐", "http", "Host Services");
    by_port(443, "HTTPS Server", "HTTPS web root", "🔒", "https", "Host Services");
    by_port(139, "SMB (NetBIOS)", "Windows/Samba file sharing", "📁", "tcp", "Host Services");
    by_port(445, "SMB File Sharing", "Windows/Samba file sharing", "📁", "tcp", "Host Services");
    by_port(631, "CUPS Print Server", "Local printing service", "🖨️", "http", "Host Services");
    by_port(1883, "MQTT Broker", "MQTT messaging broker", "📨", "mqtt", "Host Services");
    by_port(3000, "Web App :3000", "Common application port", "🌐", "http", "Host Services");
    by_port(3001, "Grafana", "Metrics and dashboard visualisation", "📊", "http", "Monitoring");
    by_port(3306, "MySQL / MariaDB", "SQL database server", "🐬", "tcp", "Host Services");
    by_port(5000, "Web App :5000", "Common application port", "🌐", "http", "Host Services");
    by_port(5001, "Dockge", "Docker Compose stack manager", "🐳", "http", "Host Services");
    by_port(5432, "PostgreSQL", "SQL database server", "🐘", "tcp", "Host Services");
    by_port(5601, "Kibana", "Log exploration UI", "📊", "http", "Monitoring");
    by_port(5900, "VNC Server", "Remote desktop (display :0)", "🖥️", "vnc", "Host Services");
    by_port(5901, "VNC Server", "Remote desktop (display :1)", "🖥️", "vnc", "Host Services");
    by_port(6080, "noVNC", "Browser-based VNC client", "🖥️", "http", "Host Services");
    by_port(6120, "Glances", "Real-time system monitoring", "📈", "http", "Monitoring");
    by_port(61208, "Glances", "Real-time system monitoring", "📈", "http", "Monitoring");
    by_port(6379, "Redis", "In-memory key/value store", "🧱", "tcp", "Host Services");
    by_port(8000, "Web App :8000", "Common development port", "🌐", "http", "Development");
    by_port(8001, "Web Terminal", "Browser-based shell", "💻", "http", "Development");
    by_port(8025, "Mailpit", "Local mail catcher UI", "📬", "http", "Development");
    by_port(8080, "HTTP Alt :8080", "Common alternative HTTP port", "🔀", "http", "Host Services");
    by_port(8081, "HTTP Alt :8081", "Common alternative HTTP port", "🔀", "http", "Host Services");
    by_port(8096, "Jellyfin", "Media server", "🎬", "http", "Host Services");
    by_port(8123, "Home Assistant", "Home automation hub", "🏠", "http", "Host Services");
    by_port(8443, "HTTPS Alt :8443", "Common alternative HTTPS port", "🔒", "https", "Host Services");
    by_port(8555, "metatron", "PhaseShift Studio metatron VM", "icons/metatron.png", "http", "Host Services");
    by_port(8888, "HTTP Alt :8888", "Notebook or ad-hoc HTTP server", "📓", "http", "Development");
    by_port(9000, "Portainer", "Container management UI", "🖥️", "http", "Host Services");
    by_port(9090, "Prometheus / Cockpit", "Metrics endpoint or server cockpit", "📈", "http", "Monitoring");
    by_port(9091, "Metrics Exporter", "Metrics scrape endpoint", "🎛️", "http", "Monitoring");
    by_port(9119, "Hermes Web", "Agent web UI", "🤖", "http", "Remote & Guest VMs");
    by_port(9122, "Chat API", "Local chat/LLM API", "💬", "http", "Host Services");
    by_port(11434, "Ollama", "Local LLM inference server", "🦙", "http", "Host Services");
    by_port(27017, "MongoDB", "Document database", "🍃", "tcp", "Host Services");
    by_port(32400, "Plex", "Media server", "▶️", "http", "Host Services");
    by_port(51820, "WireGuard", "VPN endpoint", "🔒", "tcp", "Host Services");

    // Sort by matcher length so a specific rule always beats a generic one it merely
    // contains (`dockge` is not `docker`, but `docker-socket-proxy` is). Without this,
    // whichever rule happened to be written first won, which is how Dockge ended up
    // labelled "Docker". `sort_by_key` is stable, so ties keep their table order.
    identity.sort_by_key(|r| std::cmp::Reverse(r.matcher.as_deref().unwrap_or("").len()));

    (identity, port_rules)
}

/// Write a starter `overrides.toml` from the currently discovered services, so the
/// user can rename/re-icon/group things without hunting for ports.
pub(crate) fn scaffold(services: &[SplashService]) -> String {
    let mut out = String::new();
    out.push_str(
        "# server-splash curation\n\
         #\n\
         # Rules are tried top to bottom; the first rule that matches a discovered\n\
         # service and supplies a field wins. `port` matches the listening port,\n\
         # `match` matches a substring of the container / systemd unit / process name.\n\
         # Add or edit entries here instead of editing the generated HTML.\n\
         #\n\
         # Delete a rule to fall back to the built-in catalog.\n\n",
    );
    for svc in services {
        let port = svc.port.as_deref().unwrap_or("");
        out.push_str("[[service]]\n");
        if !port.is_empty() {
            out.push_str(&format!("port = {port}\n"));
        }
        out.push_str(&format!("name = {}\n", toml_string(&svc.name)));
        out.push_str(&format!("desc = {}\n", toml_string(&svc.desc)));
        out.push_str(&format!("icon = {}\n", toml_string(&svc.icon)));
        out.push_str(&format!("protocol = {}\n", toml_string(&svc.protocol)));
        out.push_str(&format!("group = {}\n", toml_string(&svc.group)));
        out.push('\n');
    }
    out
}

/// Minimal TOML basic-string quoting.
fn toml_string(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{FetchedIcon, HttpInfo};

    fn svc(port: u16, container: Option<&str>, unit: Option<&str>, proc: Option<&str>) -> DiscoveredService {
        DiscoveredService {
            port,
            remote_reachable: true,
            proc_name: proc.map(String::from),
            pid: None,
            unit: unit.map(String::from),
            unit_desc: None,
            container: container.map(String::from),
            image: None,
            http: None,
        }
    }

    #[test]
    fn container_identity_beats_generic_port_rule() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        // Port 8080 alone is ambiguous; the container name must win.
        let z2m = svc(8080, Some("zigbee2mqtt"), None, None);
        let resolved = cat.resolve(&z2m, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "Zigbee2MQTT");
        assert_eq!(resolved.group, "Host Services");
        assert_eq!(resolved.id.as_deref(), Some("docker:zigbee2mqtt:8080"));
    }

    #[test]
    fn port_rule_names_an_unknown_process_port() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let ollama = svc(11434, None, None, Some("ollama"));
        let resolved = cat.resolve(&ollama, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "Ollama");
        assert_eq!(resolved.web_probe_url.as_deref(), Some("http://ginger.local:11434"));
        assert_eq!(resolved.port.as_deref(), Some("11434"));
    }

    #[test]
    fn unknown_port_falls_back_to_http_title() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let mut s = svc(43210, None, None, Some("mystery"));
        s.http = Some(HttpInfo {
            scheme: "http".into(),
            status: 200,
            server: None,
            title: Some("My Custom Tool".into()),
            icon: None,
        });
        let resolved = cat.resolve(&s, "host", true).unwrap();
        assert_eq!(resolved.name, "My Custom Tool");
        assert_eq!(resolved.desc, "Listening process: mystery");
        assert!(resolved.alive, "an HTTP response means it was alive at generation time");
    }

    #[test]
    fn useless_titles_are_ignored() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let mut s = svc(43211, None, None, Some("apache2"));
        s.http = Some(HttpInfo {
            scheme: "http".into(),
            status: 200,
            server: None,
            title: Some("Apache2 Ubuntu Default Page".into()),
            icon: None,
        });
        let resolved = cat.resolve(&s, "host", true).unwrap();
        assert_eq!(resolved.name, "Apache2", "falls through to the identity rule");
    }

    #[test]
    fn non_http_services_get_no_probe_url() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let ssh = svc(22, None, Some("ssh.service"), None);
        let resolved = cat.resolve(&ssh, "ginger.local", true).unwrap();
        assert_eq!(resolved.protocol, "ssh");
        assert_eq!(resolved.web_probe_url, None, "the browser cannot health-check ssh");
    }

    #[test]
    fn user_rules_override_builtins() {
        let dir = std::env::temp_dir().join("splash-catalog-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overrides.toml");
        std::fs::write(&path, "[[service]]\nport = 9090\nname = \"My Prometheus\"\nicon = \"🔥\"\n").unwrap();

        let cat = Catalog::load(&path);
        let resolved = cat.resolve(&svc(9090, None, None, None), "host", true).unwrap();
        assert_eq!(resolved.name, "My Prometheus", "user curation wins");
        assert_eq!(resolved.icon, "🔥");
        assert_eq!(resolved.desc, "Metrics endpoint or server cockpit", "unset fields still come from the builtin");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn hidden_rules_drop_the_service() {
        let dir = std::env::temp_dir().join("splash-catalog-hidden");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overrides.toml");
        std::fs::write(&path, "[[service]]\nport = 445\nhidden = true\n").unwrap();

        let cat = Catalog::load(&path);
        assert!(cat.resolve(&svc(445, None, None, None), "host", true).is_none());
        std::fs::remove_file(&path).ok();
    }

    /// Regression pin against the real target host (`ginger`): the ports below are
    /// the ones an actual scan found, and every one of them must resolve to a card
    /// that is not just "Service on :port".
    #[test]
    fn resolves_the_real_ginger_inventory() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let expected: &[(u16, &str)] = &[
            (22, "SSH Server"),
            (80, "HTTP Server"),
            (139, "SMB (NetBIOS)"),
            (445, "SMB File Sharing"),
            (1883, "MQTT Broker"),
            (3306, "MySQL / MariaDB"),
            (5001, "Dockge"),
            (61208, "Glances"),
            (8080, "HTTP Alt :8080"),
            (8888, "HTTP Alt :8888"),
            (9090, "Prometheus / Cockpit"),
            (11434, "Ollama"),
        ];
        for (port, name) in expected {
            let resolved = cat
                .resolve(&svc(*port, None, None, None), "ginger.local", true)
                .unwrap_or_else(|| panic!(":{port} should resolve to a card"));
            assert_eq!(&resolved.name, name, ":{port} was misnamed");
            assert!(!resolved.name.starts_with("Service on :"), ":{port} fell through to the generic fallback");
        }
    }

    /// Attach a favicon the way discovery would.
    fn with_favicon(mut s: DiscoveredService, ext: &str, title: Option<&str>) -> DiscoveredService {
        s.http = Some(HttpInfo {
            scheme: "http".into(),
            status: 200,
            server: None,
            title: title.map(String::from),
            icon: Some(FetchedIcon {
                bytes: vec![0x89, b'P', b'N', b'G'],
                ext: ext.to_string(),
                source: format!("http://127.0.0.1:{}/favicon.{}", s.port, ext),
            }),
        });
        s
    }

    /// Docker forwards every published port through `docker-proxy`. Treating that as
    /// the service's identity labelled Dockge — and every other container — "Docker".
    #[test]
    fn docker_proxy_does_not_masquerade_as_the_service() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));

        let mut dockge = svc(5001, Some("dockge"), None, Some("docker-proxy"));
        dockge.image = Some("louislam/dockge:1".to_string());
        let resolved = cat.resolve(&dockge, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "Dockge");
        assert_eq!(resolved.icon, "🐳");

        // A published port with no recognisable container falls through to the port
        // rule rather than being called "Docker".
        let mut anonymous = svc(3306, None, None, Some("docker-proxy"));
        anonymous.image = Some("mariadb:11.2".to_string());
        let resolved = cat.resolve(&anonymous, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "MariaDB");
        assert!(!resolved.name.contains("Docker"), "{resolved:?}");

        // And docker-proxy alone, with nothing else to go on, is not an identity.
        let bare = svc(49999, None, None, Some("docker-proxy"));
        let resolved = cat.resolve(&bare, "ginger.local", true).unwrap();
        assert_ne!(resolved.name, "Docker");
    }

    #[test]
    fn a_specific_matcher_beats_a_generic_one_wherever_it_sits_in_the_table() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let mut proxy = svc(2375, None, None, Some("docker-socket-proxy"));
        proxy.container = Some("docker-socket-proxy".to_string());
        let resolved = cat.resolve(&proxy, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "Docker Socket Proxy", "not plain \"Docker\"");
    }

    #[test]
    fn a_recognised_service_keeps_its_catalog_icon_over_its_favicon() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let apache = with_favicon(svc(80, None, None, Some("apache2")), "ico", None);
        let resolved = cat.resolve(&apache, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "Apache2");
        assert_eq!(resolved.icon, "🪶", "Apache gets the feather, not a generic globe");
        assert!(resolved.icon_specific);
        assert!(resolved.icon_data.is_none(), "no need to mirror an icon we did not use");
    }

    #[test]
    fn an_unknown_service_uses_its_own_favicon_instead_of_a_port_guess() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        // Port 8888 has a port-only rule (Jupyter 📓); a real favicon should beat it.
        let mut s = svc(8888, None, None, Some("mystery"));
        s.http = Some(HttpInfo {
            scheme: "http".into(),
            status: 200,
            server: None,
            title: Some("Some Custom Tool".into()),
            icon: Some(FetchedIcon {
                bytes: vec![0x89, b'P', b'N', b'G'],
                ext: "png".into(),
                source: "http://127.0.0.1:8888/favicon.png".into(),
            }),
        });
        let resolved = cat.resolve(&s, "ginger.local", true).unwrap();
        assert_eq!(resolved.icon, "icons/8888.png", "the favicon is mirrored locally");
        assert!(resolved.icon_data.is_some());
        assert!(resolved.icon_specific);
    }

    #[test]
    fn user_curation_beats_a_favicon() {
        let dir = std::env::temp_dir().join("splash-catalog-icon-precedence");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overrides.toml");
        std::fs::write(&path, "[[service]]\nport = 8888\nicon = \"🔥\"\n").unwrap();

        let cat = Catalog::load(&path);
        let s = with_favicon(svc(8888, None, None, None), "png", None);
        let resolved = cat.resolve(&s, "ginger.local", true).unwrap();
        assert_eq!(resolved.icon, "🔥", "an explicit choice is never overridden");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_service_with_nothing_known_falls_back_to_the_protocol_icon() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        // It answers HTTP, so the protocol default applies — but it published no icon
        // and no rule knows it.
        let mut bare = with_favicon(svc(4321, None, None, Some("mystery")), "png", None);
        if let Some(http) = bare.http.as_mut() {
            http.icon = None;
        }
        let resolved = cat.resolve(&bare, "ginger.local", true).unwrap();
        assert_eq!(resolved.protocol, "http");
        assert_eq!(resolved.icon, "🌐");
        assert!(!resolved.icon_specific, "the fallback stays open to a better suggestion");
        assert!(!resolved.name_specific, "an unidentified service can still be named by the agent");
        assert!(!resolved.desc_specific);
        assert!(resolved.icon_data.is_none());
    }

    #[test]
    fn metatron_is_recognised_from_its_container_name_or_its_page_title() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));

        // Recognised from the process/container name.
        let by_name = cat.resolve(&svc(8555, Some("metatron"), None, None), "ginger.local", true).unwrap();
        assert_eq!(by_name.name, "metatron", "the project spells it lower case, always");
        assert_eq!(by_name.icon, "icons/metatron.png");
        assert!(by_name.name_specific, "so enrichment must not re-capitalise it");
        assert!(by_name.desc_specific);

        // Recognised from the page title when the container is anonymous.
        let by_title = with_favicon(
            svc(8777, None, None, Some("weird-name")),
            "png",
            Some("metatron — PhaseShift Studio"),
        );
        let resolved = cat.resolve(&by_title, "ginger.local", true).unwrap();
        assert_eq!(resolved.name, "metatron");
        assert_eq!(resolved.icon, "icons/metatron.png", "the logo beats the favicon");
        assert!(resolved.name_specific, "a self-declared title is equally authoritative");
    }

    #[test]
    fn port_index_prefers_curation_and_covers_the_common_ports() {
        let dir = std::env::temp_dir().join("splash-port-index");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("overrides.toml");
        std::fs::write(&path, "[[service]]\nport = 9000\nname = \"My Portainer\"\n").unwrap();

        let cat = Catalog::load(&path);
        let index = cat.port_index();

        let portainer = index.iter().find(|e| e.port == 9000).unwrap();
        assert_eq!(portainer.name, "My Portainer", "curation beats the builtin");
        let ollama = index.iter().find(|e| e.port == 11434).unwrap();
        assert_eq!(ollama.name, "Ollama");
        assert_eq!(ollama.icon, "🦙", "icons travel to the browser with the name");

        // Every entry must be uniquely indexed, or the page would render twice.
        let mut ports: Vec<u16> = index.iter().map(|e| e.port).collect();
        let before = ports.len();
        ports.dedup();
        assert_eq!(ports.len(), before, "no duplicate ports");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn resync_candidates_are_sane_and_include_the_catalog_ports() {
        assert!(RESYNC_CANDIDATE_PORTS.len() > 80, "a useful probe list");
        assert!(RESYNC_CANDIDATE_PORTS.contains(&11434));
        assert!(!RESYNC_CANDIDATE_PORTS.contains(&0));
        // Reserved / never-listened-on ports would just be wasted probes.
        assert!(!RESYNC_CANDIDATE_PORTS.iter().any(|p| *p < 22));
    }

    #[test]
    fn scaffold_round_trips_through_the_parser() {
        let cat = Catalog::load(Path::new("/nonexistent/overrides.toml"));
        let resolved: Vec<SplashService> = [22u16, 11434]
            .iter()
            .filter_map(|p| cat.resolve(&svc(*p, None, None, None), "ginger.local", true))
            .collect();
        let text = scaffold(&resolved);
        let parsed: Overrides = toml::from_str(&text).expect("scaffolded TOML must parse");
        assert_eq!(parsed.service.len(), 2);
        assert_eq!(parsed.service[1].name.as_deref(), Some("Ollama"));
        assert_eq!(parsed.service[1].port, Some(11434));
    }
}

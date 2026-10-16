mod config;
mod probe;
mod wizard;
mod generator;
mod agent;
mod modules;
mod discover;
mod catalog;
mod server;

use std::path::{Path, PathBuf};

use catalog::Catalog;
use config::SplashConfig;
use discover::Discovery;
use wizard::{SplashService, WizardOutput};

const USAGE: &str = "\
server-splash — generate a quick-launch dashboard for the services on a host

USAGE:
    splash [OPTIONS]              interactive wizard (discover → curate → generate)
    splash --auto [OPTIONS]       regenerate non-interactively, e.g. from a timer
    splash --serve [OPTIONS]      keep running: regenerate on a schedule and serve it
    splash --scaffold [OPTIONS]   write a starter overrides.toml from live discovery

OPTIONS:
    -a, --auto                 Non-interactive: discover, resolve, generate, exit
        --serve                Serve the page and keep regenerating it
        --port <PORT>          Port for --serve                  [default: 6420]
        --interval <SECONDS>   Seconds between refreshes while serving [default: 900]
        --healthcheck          Exit 0 if the served page answers, else 1
        --scaffold             Write curation rules for everything discovered, then exit
        --force                Allow --scaffold to overwrite an existing overrides.toml
        --hostname <NAME>      Hostname used in links and the page heading
        --host-suffix <SUF>    Appended to a bare hostname   [default: .local]
        --title <TEXT>         Page heading and <title>
        --output <DIR>         Where to write the page        [default: ./server-splash]
        --output-file <NAME>   Page filename                  [default: index.html]
        --deploy <DIR>         Copy the finished page here, e.g. /var/www/html
        --backup-dir <DIR>     Keep copies of files a deploy overwrites
                               [default: ~/.local/share/server-splash/backups]
        --no-backup            Do not keep copies of overwritten files
        --overrides <FILE>     Curation rules file            [default: <config>/overrides.toml]
        --glances <URL>        Glances API base, enables the live GPU cards
        --exclude-port <PORT>  Never show this port (repeatable)
        --include-loopback     Also list services bound to 127.0.0.1 only
        --include-ephemeral    Also list ports 32768-60999 (test/container noise)
        --no-probe-http        Skip probing ports to classify them
        --no-agent             Never call an LLM (also skips the enrichment pass)
        --agent <URL>          OpenAI-compatible endpoint for optional enrichment
        --model <NAME>         Model to use for enrichment
        --config <FILE>        Alternate config file
    -h, --help                 Show this help
    -V, --version              Show the version

The generated page replaces the static page at http://<hostname>/ when deployed to
the web root with --output-file index.html.";

#[derive(Debug, Default)]
struct Cli {
    auto: bool,
    scaffold: bool,
    force: bool,
    help: bool,
    version: bool,
    no_agent: bool,
    no_probe_http: bool,
    include_loopback: bool,
    include_ephemeral: bool,
    no_backup: bool,
    backup_dir: Option<String>,
    host_suffix: Option<String>,
    serve: bool,
    port: Option<u16>,
    interval: Option<u64>,
    healthcheck: bool,
    agent: Option<String>,
    model: Option<String>,
    hostname: Option<String>,
    title: Option<String>,
    output_dir: Option<String>,
    output_file: Option<String>,
    deploy: Option<String>,
    overrides: Option<String>,
    glances: Option<String>,
    config: Option<String>,
    exclude_ports: Vec<u16>,
}

impl Cli {
    fn parse(args: &[String]) -> Result<Cli, String> {
        let mut cli = Cli::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value_for = |name: &str| -> Result<String, String> {
                it.next().cloned().ok_or_else(|| format!("{name} requires a value"))
            };
            match arg.as_str() {
                "-a" | "--auto" => cli.auto = true,
                "--scaffold" => cli.scaffold = true,
                "--force" => cli.force = true,
                "-h" | "--help" => cli.help = true,
                "-V" | "--version" => cli.version = true,
                "--no-agent" => cli.no_agent = true,
                "--no-probe-http" => cli.no_probe_http = true,
                "--include-loopback" => cli.include_loopback = true,
                "--include-ephemeral" => cli.include_ephemeral = true,
                "--no-backup" => cli.no_backup = true,
                "--serve" => cli.serve = true,
                "--healthcheck" => cli.healthcheck = true,
                "--port" => {
                    let raw = value_for("--port")?;
                    cli.port = Some(raw.parse().map_err(|_| format!("--port expects a number, got {raw:?}"))?);
                }
                "--interval" => {
                    let raw = value_for("--interval")?;
                    cli.interval = Some(raw.parse().map_err(|_| format!("--interval expects seconds, got {raw:?}"))?);
                }
                "--backup-dir" => cli.backup_dir = Some(value_for("--backup-dir")?),
                "--host-suffix" => cli.host_suffix = Some(value_for("--host-suffix")?),
                "--agent" => cli.agent = Some(value_for("--agent")?),
                "--model" => cli.model = Some(value_for("--model")?),
                "--hostname" => cli.hostname = Some(value_for("--hostname")?),
                "--title" => cli.title = Some(value_for("--title")?),
                "--output" => cli.output_dir = Some(value_for("--output")?),
                "--output-file" => cli.output_file = Some(value_for("--output-file")?),
                "--deploy" => cli.deploy = Some(value_for("--deploy")?),
                "--overrides" => cli.overrides = Some(value_for("--overrides")?),
                "--glances" => cli.glances = Some(value_for("--glances")?),
                "--config" => cli.config = Some(value_for("--config")?),
                "--exclude-port" => {
                    let raw = value_for("--exclude-port")?;
                    let port: u16 = raw
                        .parse()
                        .map_err(|_| format!("--exclude-port expects a number, got {raw:?}"))?;
                    cli.exclude_ports.push(port);
                }
                other => return Err(format!("unknown argument {other:?} (try --help)")),
            }
        }
        Ok(cli)
    }

    /// Layer the command line on top of the persisted config.
    fn apply(&self, mut cfg: SplashConfig) -> SplashConfig {
        if let Some(v) = &self.agent { cfg.agent_url = v.clone(); }
        if let Some(v) = &self.model { cfg.model = Some(v.clone()); }
        if let Some(v) = &self.hostname { cfg.hostname = Some(v.clone()); }
        if let Some(v) = &self.title { cfg.title = Some(v.clone()); }
        if let Some(v) = &self.output_dir { cfg.output_dir = Some(v.clone()); }
        if let Some(v) = &self.output_file { cfg.output_file = Some(v.clone()); }
        if let Some(v) = &self.deploy { cfg.deploy_path = Some(v.clone()); }
        if let Some(v) = &self.overrides { cfg.overrides_file = Some(v.clone()); }
        if let Some(v) = &self.glances { cfg.glances_api_base = Some(v.clone()); }
        if self.no_agent { cfg.no_agent = Some(true); }
        if self.no_probe_http { cfg.probe_http = Some(false); }
        if self.include_loopback { cfg.include_loopback = Some(true); }
        if self.include_ephemeral { cfg.include_ephemeral = Some(true); }
        if self.no_backup { cfg.no_backup = Some(true); }
        if let Some(d) = &self.backup_dir { cfg.backup_dir = Some(d.clone()); }
        if let Some(x) = &self.host_suffix { cfg.host_suffix = Some(x.clone()); }
        if let Some(p) = self.port { cfg.serve_port = Some(p); }
        if let Some(i) = self.interval { cfg.interval_secs = Some(i); }
        if self.serve { cfg.serve_port = Some(cfg.serve_port.unwrap_or(config::DEFAULT_SERVE_PORT)); }
        if !self.exclude_ports.is_empty() {
            let mut ports = cfg.excluded_ports();
            ports.extend(self.exclude_ports.iter().copied());
            ports.sort_unstable();
            ports.dedup();
            cfg.exclude_ports = Some(ports);
        }
        cfg
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match Cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m {e}");
            std::process::exit(2);
        }
    };

    if cli.help {
        println!("{USAGE}");
        return;
    }
    if cli.version {
        println!("server-splash {} (build {})", env!("CARGO_PKG_VERSION"), build_stamp());
        return;
    }

    // Answered before the banner: a health probe runs every few seconds and should
    // produce no output at all.
    if cli.healthcheck {
        let port = cli
            .port
            .or_else(|| std::env::var("SPLASH_PORT").ok().and_then(|p| p.parse().ok()))
            .unwrap_or(config::DEFAULT_SERVE_PORT);
        if !server::check(port) {
            eprintln!("nothing is serving on port {port}");
            std::process::exit(1);
        }
        return;
    }

    print_banner();

    let mut cfg = match &cli.config {
        Some(path) => load_config_from(Path::new(path)),
        None => SplashConfig::load(),
    };
    cfg.apply_env();
    // Precedence is CLI > environment > config file > defaults.
    let cfg = cli.apply(cfg);

    let result: Result<(), String> = if let Some(port) = cfg.serve_port() {
        run_serve(&cfg, port)
    } else if cli.scaffold {
        run_scaffold(&cfg, cli.force)
    } else if cli.auto {
        run_auto(&cfg).and_then(|out| generate_and_deploy(&cfg, out))
    } else if !atty::is(atty::Stream::Stdin) {
        // No terminal to prompt on — a container, a cron job, a pipe. Running the
        // interactive wizard here would hang or fail with nothing useful to say.
        eprintln!("\x1b[33mnote\x1b[0m: no terminal, generating once (pass --serve to keep it running)");
        run_auto(&cfg).and_then(|out| generate_and_deploy(&cfg, out))
    } else {
        wizard::run(&cfg)
            .map_err(|e| e.to_string())
            .and_then(|out| generate_and_deploy(&cfg, out))
    };

    if let Err(e) = result {
        eprintln!("\n\x1b[1;31merror:\x1b[0m {e}");
        std::process::exit(1);
    }
}

/// When this binary was compiled, from `build.rs`. Used by `--version`, by
/// `dashboard.json`, and in the page footer, so a stale install is visible instead of
/// silently producing an "unchanged" page.
pub(crate) fn build_stamp() -> &'static str {
    option_env!("SPLASH_BUILD").unwrap_or("unknown")
}

fn load_config_from(path: &Path) -> SplashConfig {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str::<SplashConfig>(&text)
            .map(|mut c| { c.source_path = Some(path.to_path_buf()); c })
            .unwrap_or_else(|e| {
            eprintln!("\x1b[1;31merror:\x1b[0m {} is not valid TOML: {e}", path.display());
            std::process::exit(2);
        }),
        Err(e) => {
            eprintln!("\x1b[1;31merror:\x1b[0m cannot read {}: {e}", path.display());
            std::process::exit(2);
        }
    }
}

/// Discovery options implied by the config.
fn discovery_options(cfg: &SplashConfig) -> discover::Options {
    discover::Options {
        include_loopback: cfg.include_loopback(),
        exclude_ports: cfg.excluded_ports(),
        include_ephemeral: cfg.include_ephemeral(),
        probe_http: cfg.probe_http(),
        http_timeout: std::time::Duration::from_millis(cfg.http_timeout_ms()),
        // The kernel's own range, so a port just above it (Glances, 61208) survives.
        ..discover::Options::default()
    }
}

/// Run discovery and resolve every endpoint through the catalog.
fn build_services(cfg: &SplashConfig, hostname: &str) -> (Discovery, Vec<SplashService>) {
    let discovery = discover::run(&discovery_options(cfg));
    let catalog = Catalog::load(&cfg.get_overrides_path());

    let mut services: Vec<SplashService> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for svc in &discovery.services {
        let Some(resolved) = catalog.resolve(svc, hostname, true) else { continue };
        // Guard against two sources (docker + ss) yielding the same curation id.
        let key = resolved.id.clone().unwrap_or_else(|| resolved.name.clone());
        if seen.insert(key) {
            services.push(resolved);
        }
    }
    (discovery, services)
}

/// Non-interactive regeneration: discover → resolve → optionally enrich → return.
fn run_auto(cfg: &SplashConfig) -> Result<WizardOutput, String> {
    eprintln!("\x1b[1;36mDiscovering services\x1b[0m");
    let (discovery, mut services) = build_services(cfg, cfg.hostname.as_deref().unwrap_or(""));

    // Links are followed by a browser, so a bare hostname has to be made resolvable.
    let hostname = discover::qualify_host(
        cfg.hostname
            .clone()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| discovery.machine.hostname.clone())
            .as_str(),
        &cfg.host_suffix(),
    );

    // The links were built with the pre-discovery hostname; rebuild them now that we
    // know the real one.
    if hostname != cfg.hostname.clone().unwrap_or_default() {
        let catalog = Catalog::load(&cfg.get_overrides_path());
        services = discovery
            .services
            .iter()
            .filter_map(|s| catalog.resolve(s, &hostname, true))
            .collect();
    }

    eprintln!(
        "  found \x1b[1;32m{}\x1b[0m listening service(s); {} on the page",
        discovery.services.len(),
        services.len()
    );

    if cfg.agent_enabled() {
        enrich_with_agent(cfg, &hostname, &mut services);
    }

    if discover::looks_like_container_id(&hostname) {
        eprintln!(
            "  \x1b[33mwarning\x1b[0m: the hostname resolved to \"{hostname}\", which looks like a"
        );
        eprintln!(
            "           container id. Set SPLASH_HOSTNAME (or --hostname) to the name you"
        );
        eprintln!("           reach this machine by, or links on the page will not resolve.");
    }

    let subtitle = if cfg.get_subtitle().is_empty() {
        discovery.machine.subtitle()
    } else {
        cfg.get_subtitle()
    };
    if !subtitle.is_empty() {
        eprintln!("  subtitle: \x1b[36m{subtitle}\x1b[0m");
    }

    let glances_api_base = resolve_glances(cfg, &discovery, &hostname);
    let title = cfg.get_title(&hostname);

    Ok(WizardOutput {
        output_dir: PathBuf::from(cfg.get_output_dir("./server-splash")),
        output_file: cfg.get_output_file(),
        title,
        subtitle,
        ip_display: discovery.machine.ip.clone(),
        hostname,
        selected_services: services,
        _probe_results: Vec::new(),
        selected_modules: cfg.selected_modules(),
        glances_api_base,
        deploy_path: cfg.deploy_path.clone(),
        extra_cards: Vec::new(),
        gpus: discovery.machine.gpus.clone(),
        sys_chips: discovery.machine.stat_chips(),
        speedtest_bytes: cfg.speedtest_bytes(),
        overrides_path: cfg.get_overrides_path(),
        inline_icons: cfg.inline_icons(),
    })
}

/// Where the page should fetch GPU stats from.
///
/// This URL is used by the *browser*, not by the generator, so it must name the
/// dashboard's host — never `localhost`, which the browser reads as its own machine.
fn resolve_glances(cfg: &SplashConfig, discovery: &Discovery, hostname: &str) -> Option<String> {
    if let Some(url) = cfg.glances_api_base.clone().filter(|u| !u.trim().is_empty()) {
        if discover::is_viewer_localhost(&url) {
            eprintln!(
                "  \x1b[33mwarning\x1b[0m: glances URL {url} points at the *viewer's* machine;"
            );
            eprintln!(
                "           the browser resolves it, not this host. Use http://{hostname}:{} instead.",
                port_of(&url).unwrap_or(61208)
            );
        }
        return Some(url);
    }

    // Auto-detect from the discovered Glances service, addressed by display hostname.
    discovery
        .services
        .iter()
        .find(|s| s.identity().to_lowercase().contains("glances"))
        .map(|s| format!("http://{hostname}:{}", s.port))
}

/// Pull the port out of a URL, for the warning above.
fn port_of(url: &str) -> Option<u16> {
    let rest = url.trim().trim_start_matches("http://").trim_start_matches("https://");
    let authority = rest.split('/').next()?;
    authority.rsplit_once(':')?.1.parse().ok()
}

/// Ask the configured LLM to name services that discovery could not identify.
///
/// This is strictly best-effort: the page is complete without it, so every failure
/// path just prints a note and carries on.
fn enrich_with_agent(cfg: &SplashConfig, hostname: &str, services: &mut [SplashService]) {
    let unresolved: Vec<usize> = services
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            s.name.starts_with("Service on :")
                || s.desc.starts_with("Listening process:")
                || s.desc.starts_with("Listening on TCP port")
        })
        .map(|(i, _)| i)
        .collect();

    if unresolved.is_empty() {
        return;
    }
    if !agent::is_openai_compatible(&cfg.agent_url) {
        eprintln!(
            "  \x1b[33mnote\x1b[0m: {} unidentified service(s); no agent at {}",
            unresolved.len(),
            cfg.agent_url
        );
        return;
    }

    let model = match cfg.model.clone().or_else(|| agent::get_default_model(&cfg.agent_url)) {
        Some(m) => m,
        None => {
            eprintln!("  \x1b[33mnote\x1b[0m: agent reachable but no model available");
            return;
        }
    };

    eprintln!(
        "  asking \x1b[36m{model}\x1b[0m to identify {} service(s)…",
        unresolved.len()
    );

    let catalogue: Vec<String> = services
        .iter()
        .filter(|s| s.port.is_some())
        .map(|s| {
            format!(
                "  :{} {} — {}",
                s.port.as_deref().unwrap_or("?"),
                s.name,
                s.source.as_deref().unwrap_or("")
            )
        })
        .collect();

    let unknown: Vec<String> = unresolved
        .iter()
        .map(|&i| {
            format!(
                "  :{} {}",
                services[i].port.as_deref().unwrap_or("?"),
                services[i].source.as_deref().unwrap_or("")
            )
        })
        .collect();

    let prompt = format!(
        "Host: {hostname}\n\nServices already identified:\n{}\n\nIdentify these services:\n{}\n\n\
         Reply with ONLY a JSON array of objects with keys \"port\" (string), \"name\" (max 4 words), \
         \"desc\" (5-12 words), \"icon\" (one emoji). No markdown fences.\n\n\
         The icon must be distinctive and specific to that piece of software (PostgreSQL -> 🐘, \
         Redis -> 🧱, Home Assistant -> 🏠). Do NOT use a generic globe, plug, box or gear: if you \
         cannot think of a specific icon, omit the \"icon\" key entirely.",
        catalogue.join("\n"),
        unknown.join("\n")
    );

    match agent::complete(&cfg.agent_url, &model, &prompt) {
        Ok(text) => {
            let applied = apply_suggestions(&text, services);
            eprintln!("  agent identified \x1b[1;32m{applied}\x1b[0m service(s)");
        }
        Err(e) => eprintln!("  \x1b[33mnote\x1b[0m: agent enrichment skipped ({e})"),
    }
}

/// Icons a model reaches for when it does not actually know the service. Accepting
/// these would replace a real favicon or a deliberate catalog choice with the same
/// wireframe globe the user is trying to get rid of.
const GENERIC_ICONS: &[&str] = &["🌐", "🔌", "🛠️", "⚙️", "📦", "❓", "🔗", "🖥", "💻"];

/// Merge an LLM JSON reply into the service list, keyed by port.
fn apply_suggestions(text: &str, services: &mut [SplashService]) -> usize {
    // Models add prose and fences despite instructions; keep just the array.
    let start = match text.find('[') { Some(i) => i, None => return 0 };
    let end = match text.rfind(']') {
        Some(i) if i >= start => i + 1,
        _ => return 0,
    };
    let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&text[start..end]) else { return 0 };

    let mut applied = 0;
    for item in arr {
        let port = item
            .get("port")
            .and_then(|v| v.as_str().map(String::from).or_else(|| v.as_u64().map(|n| n.to_string())));
        let Some(port) = port else { continue };
        let Some(svc) = services.iter_mut().find(|s| s.port.as_deref() == Some(port.as_str())) else { continue };

        // Only rename something we could not identify. A model re-spelling a name it
        // was not asked about (`metatron` -> `Metatron`) or flattening several
        // services to one label (`Docker`, `Docker`, `Docker`) is a regression.
        if !svc.name_specific {
            if let Some(name) = item.get("name").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
                svc.name = name.trim().to_string();
                svc.name_specific = true;
            }
        }
        if !svc.desc_specific {
            if let Some(desc) = item.get("desc").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
                svc.desc = desc.trim().to_string();
                svc.desc_specific = true;
            }
        }
        // The model may only supply an icon where we have nothing better than the
        // per-protocol default, and only if it is not itself generic.
        if !svc.icon_specific {
            if let Some(icon) = item.get("icon").and_then(|v| v.as_str()) {
                let icon = icon.trim();
                if !icon.is_empty() && !GENERIC_ICONS.contains(&icon) {
                    svc.icon = icon.to_string();
                    svc.icon_specific = true;
                }
            }
        }
        applied += 1;
    }
    applied
}

/// Generate once, serve the result, and keep it fresh in the background.
///
/// This is what makes the container a service rather than a one-shot job: nothing else
/// has to own the page, and there is no web root or file-permission dance.
fn run_serve(cfg: &SplashConfig, port: u16) -> Result<(), String> {
    let out_dir = PathBuf::from(cfg.get_output_dir("./server-splash"));

    // The first pass never calls the LLM. Serving must not wait on a model: this makes
    // the page available in about a second, and means a slow, hanging or entirely
    // absent endpoint can never keep the dashboard from coming up. (It did exactly
    // that: a container sat unresponsive for minutes waiting on a local model to name
    // five services.)
    let fast = SplashConfig { no_agent: Some(true), ..cfg.clone() };
    let out = run_auto(&fast)?;
    ensure_port_available(port, &out.selected_services)?;
    generate_and_deploy(&fast, out)?;

    let interval = cfg.interval_secs();
    if interval > 0 {
        let worker = cfg.clone();
        std::thread::spawn(move || {
            // If a model is configured, improve on the fast pass straight away rather
            // than making the user wait a whole interval for better names. The page is
            // already serving, so a slow answer costs nothing.
            if worker.agent_enabled() {
                refresh(&worker);
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(interval));
                refresh(&worker);
            }
        });
    }

    println!(
        "\n\x1b[1;32mServing\x1b[0m {} on http://0.0.0.0:{port}/{}",
        out_dir.display(),
        if interval > 0 { format!(" (refreshing every {interval}s)") } else { String::new() }
    );
    server::serve(&out_dir, port)
}

/// One regeneration, with failures reported rather than fatal.
fn refresh(cfg: &SplashConfig) {
    match run_auto(cfg).and_then(|out| generate_and_deploy(cfg, out)) {
        Ok(()) => {}
        Err(e) => eprintln!("  \x1b[33mrefresh failed\x1b[0m: {e}"),
    }
}

/// Refuse to serve on a port something else already owns.
///
/// Docker would report "address already in use" and crash-loop, which tells a user
/// nothing. Discovery already knows what is listening, so the error can name it and
/// suggest a port that is actually free.
fn ensure_port_available(port: u16, services: &[SplashService]) -> Result<(), String> {
    if std::net::TcpListener::bind(("0.0.0.0", port)).is_ok() {
        return Ok(());
    }

    let owner = services.iter().find(|s| s.port_num() == Some(port));
    let mut message = match owner {
        Some(svc) => format!(
            "port {port} is already in use by {}\n       ({})",
            svc.name,
            svc.source.as_deref().unwrap_or("source unknown")
        ),
        None => format!("port {port} is already in use"),
    };
    match suggest_free_port(services) {
        Some(free) => message.push_str(&format!(
            "\n       choose another port:  SPLASH_PORT={free}   (or --port {free})"
        )),
        None => message.push_str("\n       choose another port with SPLASH_PORT (or --port)"),
    }
    Err(message)
}

/// A port that is neither already serving something nor bindable by us.
fn suggest_free_port(services: &[SplashService]) -> Option<u16> {
    const CANDIDATES: [u16; 6] = [6420, 6421, 9080, 18080, 28080, 38080];
    CANDIDATES.into_iter().find(|candidate| {
        !services.iter().any(|s| s.port_num() == Some(*candidate))
            && std::net::TcpListener::bind(("0.0.0.0", *candidate)).is_ok()
    })
}

/// Write a starter curation file from what is currently running.
fn run_scaffold(cfg: &SplashConfig, force: bool) -> Result<(), String> {
    let path = cfg.get_overrides_path();
    if path.exists() && !force {
        return Err(format!(
            "{} already exists — re-run with --force to overwrite it",
            path.display()
        ));
    }
    let hostname = cfg.hostname.clone().unwrap_or_default();
    let (_, services) = build_services(cfg, &hostname);
    if services.is_empty() {
        return Err("no services discovered — nothing to write".to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, catalog::scaffold(&services))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    println!(
        "\nWrote curation rules for {} service(s) to {}",
        services.len(),
        path.display()
    );
    println!("Edit that file to rename/re-icon/group things, then re-run `splash --auto`.");
    Ok(())
}

/// Generate the page, build any selected sub-dashboards, and deploy if asked.
fn generate_and_deploy(cfg: &SplashConfig, mut out: WizardOutput) -> Result<(), String> {
    // Sub-dashboard cards are rendered before the page so they land inside the
    // template's MODULES slot rather than being spliced in afterwards.
    let all_modules = modules::all_modules();
    for name in &out.selected_modules {
        if let Some(module) = all_modules.iter().find(|m| &m.name == name) {
            out.extra_cards.push(generator::module_card(
                &module.url_prefix,
                &module.icon,
                &module.name,
                &module.description,
                module.default_port,
            ));
        }
    }

    let page = generator::generate(&out)?;

    for name in &out.selected_modules {
        if let Some(module) = all_modules.iter().find(|m| &m.name == name) {
            match (module.generate)(
                &out.output_dir,
                &out.hostname,
                module.default_port,
                module.default_service_port,
                None,
            ) {
                Ok(path) => println!("{} generated at: {path}", module.name),
                Err(e) => eprintln!("\nError generating {}: {e}", module.name),
            }
        }
    }

    println!("\n\x1b[1;32mSplash page generated at:\x1b[0m {page}");

    let deploy_target = out.deploy_path.as_deref().map(PathBuf::from);
    if let Some(target) = deploy_target {
        if target != out.output_dir {
            println!("\n\x1b[1;36mDeploying to:\x1b[0m {}", target.display());
            deploy(&out.output_dir, &target, cfg.backup_dir().as_deref())?;
            println!("\x1b[1;32mDeployed\x1b[0m → {}/{}\n", target.display(), out.output_file);
        }
    }
    Ok(())
}

/// Files that exist to debug the generator and must never end up in a web root —
/// the old version copied the whole output directory, publishing raw `systemctl` and
/// `docker ps` output to anyone who could reach the site.
const NOT_DEPLOYED: &[&str] = &[
    "splash-state.json",
    "agent-response.json",
    "agent-suggestions.json",
    "dashboard.json",
    "splash-meta.json",
];

fn deploy(src: &Path, dst: &Path, backup_root: Option<&Path>) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("Failed to create deploy dir: {e}"))?;

    // Preserve whatever we are about to overwrite. The dashboard at `index.html` is
    // often hand-maintained, and a regeneration should never be the thing that
    // destroys it.
    if let Some(root) = backup_root {
        let dir = root.join(timestamp());
        let count = backup_existing(src, dst, &dir, Path::new(""))?;
        if count > 0 {
            println!(
                "  \x1b[1;32mbacked up\x1b[0m {count} existing file(s) to {}",
                dir.display()
            );
            println!("  restore with: cp -r {}/. {}", dir.display(), dst.display());
        }
    }

    copy_dir_contents(src, dst)
}

/// `2026-10-16_155635` — a sortable, collision-resistant directory name.
fn timestamp() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d_%H%M%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "backup".to_string())
}

/// Mirror into `backup_dir` every file in `src` that already exists under `dst`.
fn backup_existing(src: &Path, dst: &Path, backup_dir: &Path, rel: &Path) -> Result<usize, String> {
    let mut count = 0;
    let entries = std::fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let name = entry.file_name();
        if NOT_DEPLOYED.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let rel_child = rel.join(&name);
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            count += backup_existing(&src_path, &dst_path, backup_dir, &rel_child)?;
        } else if dst_path.is_file() {
            let target = backup_dir.join(&rel_child);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
            }
            std::fs::copy(&dst_path, &target)
                .map_err(|e| format!("Failed to back up {}: {e}", dst_path.display()))?;
            println!("  kept {}", rel_child.display());
            count += 1;
        }
    }
    Ok(count)
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();
        if NOT_DEPLOYED.contains(&name_str.as_str()) {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if src_path.is_dir() {
            // Sub-dashboards live in their own directories, so the target has to be
            // created before recursing — otherwise every file inside fails to copy.
            std::fs::create_dir_all(&dst_path)
                .map_err(|e| format!("Failed to create {}: {e}", dst_path.display()))?;
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("Failed to copy {}: {e}", src_path.display()))?;
        }
    }
    Ok(())
}

/// The ASCII banner, printed before anything else.
fn print_banner() {
    let raw = include_str!("header.txt");
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            println!();
            continue;
        }
        if i == 5 && line.contains("PhaseShift") {
            let idx = line.find("PhaseShift").unwrap_or(0);
            let prefix = &line[..idx];
            let studio = "\x1b[38;5;214m\x1b[1mPhaseShift Studio\x1b[0m";
            let art: String = prefix
                .chars()
                .map(|c| match c {
                    ' ' => " ".to_string(),
                    _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
                })
                .collect();
            println!("{art}{studio}");
        } else {
            let colored: String = line
                .chars()
                .map(|c| match c {
                    ' ' => " ".to_string(),
                    _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
                })
                .collect();
            println!("{colored}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_free_port_is_accepted_and_a_busy_one_is_explained() {
        // Hold a port for the duration of the test.
        let held = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let busy = held.local_addr().unwrap().port();

        let services = vec![SplashService {
            name: "nginx".to_string(),
            port: Some(busy.to_string()),
            source: Some("docker container my-stack".to_string()),
            ..SplashService::blank()
        }];

        let err = ensure_port_available(busy, &services).unwrap_err();
        assert!(err.contains("already in use"), "{err}");
        assert!(err.contains("nginx"), "the error should name the occupant: {err}");
        assert!(err.contains("my-stack"), "and its source: {err}");
        assert!(err.contains("SPLASH_PORT="), "and how to fix it: {err}");

        // An unoccupied port is fine.
        assert!(ensure_port_available(0, &[]).is_ok(), "port 0 binds anything");
    }

    #[test]
    fn the_default_serve_port_is_not_a_contended_one() {
        assert_eq!(config::DEFAULT_SERVE_PORT, 6420);
        assert_ne!(config::DEFAULT_SERVE_PORT, 8080, "8080 is usually nginx");
        // Compile-time facts: these fail the build, not a test run, if the default is
        // ever changed to something unsuitable.
        const {
            assert!(config::DEFAULT_SERVE_PORT > 1024, "must not be a privileged port");
            // Spelled with comparisons rather than `contains`: the range method is not
            // const, and this check is deliberately a build failure.
            assert!(
                config::DEFAULT_SERVE_PORT < 32768 || config::DEFAULT_SERVE_PORT > 60999,
                "must be outside the ephemeral range, or it could be randomly taken"
            );
        }
    }

    fn parse(args: &[&str]) -> Result<Cli, String> {
        Cli::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_auto_and_deploy() {
        let cli = parse(&["--auto", "--deploy", "/var/www/html", "--output-file", "index.html"]).unwrap();
        assert!(cli.auto);
        assert_eq!(cli.deploy.as_deref(), Some("/var/www/html"));
        assert_eq!(cli.output_file.as_deref(), Some("index.html"));
    }

    #[test]
    fn collects_repeated_exclude_ports() {
        let cli = parse(&["--auto", "--exclude-port", "80", "--exclude-port", "443"]).unwrap();
        assert_eq!(cli.exclude_ports, vec![80, 443]);
    }

    #[test]
    fn rejects_unknown_flags_and_bad_values() {
        assert!(parse(&["--nope"]).is_err());
        assert!(parse(&["--exclude-port", "http"]).is_err());
        assert!(parse(&["--deploy"]).is_err(), "a flag needing a value must not silently pass");
    }

    #[test]
    fn cli_overrides_config_and_merges_exclude_ports() {
        let cfg = SplashConfig {
            hostname: Some("from-config".into()),
            exclude_ports: Some(vec![22]),
            ..SplashConfig::default()
        };
        let cli = parse(&["--hostname", "ginger.local", "--exclude-port", "80", "--no-agent"]).unwrap();
        let merged = cli.apply(cfg);
        assert_eq!(merged.hostname.as_deref(), Some("ginger.local"));
        assert_eq!(merged.excluded_ports(), vec![22, 80]);
        assert!(!merged.agent_enabled());
    }

    #[test]
    fn debug_files_are_never_deployed() {
        let src = std::env::temp_dir().join("splash-deploy-src");
        let dst = std::env::temp_dir().join("splash-deploy-dst");
        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&dst).ok();
        std::fs::create_dir_all(&src).unwrap();

        std::fs::write(src.join("index.html"), "<html>").unwrap();
        std::fs::write(src.join("splash-state.json"), r#"{"services":[]}"#).unwrap();
        std::fs::write(src.join("agent-response.json"), "raw systemctl output").unwrap();
        std::fs::write(src.join("dashboard.json"), "{}").unwrap();
        std::fs::create_dir_all(src.join("ollama-ui")).unwrap();
        std::fs::write(src.join("ollama-ui").join("index.html"), "<html>").unwrap();

        deploy(&src, &dst, None).unwrap();

        assert!(dst.join("index.html").exists());
        assert!(dst.join("ollama-ui").join("index.html").exists(), "sub-dashboards still deploy");
        assert!(!dst.join("splash-state.json").exists(), "state must not be published");
        assert!(!dst.join("agent-response.json").exists(), "raw probe output must not be published");
        assert!(!dst.join("dashboard.json").exists());

        std::fs::remove_dir_all(&src).ok();
        std::fs::remove_dir_all(&dst).ok();
    }

    #[test]
    fn deploy_backs_up_what_it_overwrites() {
        let root = std::env::temp_dir().join("splash-backup-test");
        let src = root.join("src");
        let dst = root.join("www");
        let backups = root.join("backups");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::create_dir_all(dst.join("ollama-ui")).unwrap();

        // The hand-maintained page we are about to replace, and a pristine new one.
        std::fs::write(dst.join("index.html"), "HAND EDITED, DO NOT LOSE ME").unwrap();
        std::fs::write(dst.join("ollama-ui").join("index.html"), "OLD MODULE").unwrap();
        std::fs::write(src.join("index.html"), "GENERATED").unwrap();
        std::fs::create_dir_all(src.join("ollama-ui")).unwrap();
        std::fs::write(src.join("ollama-ui").join("index.html"), "NEW MODULE").unwrap();
        // A brand-new file with nothing to preserve.
        std::fs::write(src.join("favicon.ico"), "NEW").unwrap();

        deploy(&src, &dst, Some(&backups)).unwrap();

        assert_eq!(std::fs::read_to_string(dst.join("index.html")).unwrap(), "GENERATED");
        assert_eq!(std::fs::read_to_string(dst.join("favicon.ico")).unwrap(), "NEW");

        // Exactly one timestamped run directory containing both overwritten files.
        let runs: Vec<_> = std::fs::read_dir(&backups).unwrap().map(|e| e.unwrap().path()).collect();
        assert_eq!(runs.len(), 1, "one directory per deploy run: {runs:?}");
        let run = &runs[0];
        assert_eq!(
            std::fs::read_to_string(run.join("index.html")).unwrap(),
            "HAND EDITED, DO NOT LOSE ME",
            "the overwritten page must be recoverable"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("ollama-ui").join("index.html")).unwrap(),
            "OLD MODULE",
            "nested files are preserved too"
        );
        assert!(!run.join("favicon.ico").exists(), "nothing to back up for a new file");

        // Backups must never be written into the web root.
        assert!(!dst.join("backups").exists());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn backups_are_skipped_when_disabled() {
        let root = std::env::temp_dir().join("splash-nobackup-test");
        let src = root.join("src");
        let dst = root.join("www");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("index.html"), "NEW").unwrap();
        std::fs::write(dst.join("index.html"), "OLD").unwrap();

        deploy(&src, &dst, None).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("index.html")).unwrap(), "NEW");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn agent_suggestions_are_matched_by_port() {
        let mut services = vec![
            SplashService { port: Some("8888".into()), name: "Service on :8888".into(), ..SplashService::blank() },
            SplashService { port: Some("9090".into()), name: "Service on :9090".into(), ..SplashService::blank() },
        ];
        let reply = "```json\n[{\"port\": 8888, \"name\": \"Jupyter\", \"desc\": \"Notebook server\", \"icon\": \"📓\"}]\n```";
        assert_eq!(apply_suggestions(reply, &mut services), 1);
        assert_eq!(services[0].name, "Jupyter");
        assert_eq!(services[0].icon, "📓");
        assert_eq!(services[1].name, "Service on :9090", "untouched ports keep their fallback name");
    }

    #[test]
    fn agent_cannot_rename_a_service_we_identified() {
        let mut services = vec![
            // Identified by the catalog: "metatron" is the project's own spelling.
            SplashService {
                port: Some("8555".into()),
                name: "metatron".into(),
                desc: "PhaseShift Studio metatron VM".into(),
                name_specific: true,
                desc_specific: true,
                ..SplashService::blank()
            },
            // Nothing identified it.
            SplashService {
                port: Some("4321".into()),
                name: "Service on :4321".into(),
                desc: "Listening on TCP port 4321".into(),
                ..SplashService::blank()
            },
        ];
        let reply = r#"[
            {"port":"8555","name":"Metatron","desc":"Some other description"},
            {"port":"4321","name":"Widget Server","desc":"Serves widgets"}
        ]"#;
        let applied = apply_suggestions(reply, &mut services);

        assert_eq!(services[0].name, "metatron", "a model must not re-capitalise a known name");
        assert_eq!(services[0].desc, "PhaseShift Studio metatron VM", "nor replace a curated description");
        assert_eq!(services[1].name, "Widget Server", "an unidentified service can still be named");
        assert_eq!(services[1].desc, "Serves widgets");
        assert_eq!(applied, 2, "both ports were reported on");
    }

    #[test]
    fn agent_may_improve_a_fallback_description_only() {
        let mut services = vec![SplashService {
            port: Some("9999".into()),
            name: "Hindsight".into(),
            desc: "Docker image foo/bar".into(),
            name_specific: true,
            desc_specific: false, // came from a fallback, so it is worth improving
            ..SplashService::blank()
        }];
        apply_suggestions(r#"[{"port":"9999","name":"Something Else","desc":"Agent memory store"}]"#, &mut services);
        assert_eq!(services[0].name, "Hindsight", "the name stays");
        assert_eq!(services[0].desc, "Agent memory store", "the fallback description is improved");
    }

    #[test]
    fn agent_cannot_replace_a_specific_icon_with_a_generic_one() {
        let mut services = vec![
            // A known service: icon_specific was set by the catalog.
            SplashService {
                port: Some("11434".into()),
                name: "Ollama".into(),
                icon: "🦙".into(),
                icon_specific: true,
                ..SplashService::blank()
            },
            // An unknown service sitting on the generic globe.
            SplashService {
                port: Some("4321".into()),
                name: "Service on :4321".into(),
                icon: "🌐".into(),
                icon_specific: false,
                ..SplashService::blank()
            },
        ];
        let reply = r#"[{"port":"11434","icon":"🌐"},{"port":"4321","icon":"🎛️"}]"#;
        apply_suggestions(reply, &mut services);

        assert_eq!(services[0].icon, "🦙", "a known service keeps its curated icon");
        assert_eq!(services[1].icon, "🎛️", "an unknown service may be given a specific icon");
    }

    #[test]
    fn agent_is_ignored_when_it_offers_a_generic_icon() {
        let mut services = vec![SplashService {
            port: Some("4321".into()),
            icon: "🌐".into(),
            icon_specific: false,
            ..SplashService::blank()
        }];
        apply_suggestions(r#"[{"port":"4321","icon":"🌐"}]"#, &mut services);
        assert_eq!(services[0].icon, "🌐");
        assert!(!services[0].icon_specific, "a generic suggestion must not lock the icon");
    }

    #[test]
    fn malformed_agent_replies_are_ignored() {
        let mut services = vec![SplashService { port: Some("8888".into()), ..SplashService::blank() }];
        assert_eq!(apply_suggestions("I could not determine that.", &mut services), 0);
        assert_eq!(apply_suggestions("[{\"port\":", &mut services), 0);
    }
}

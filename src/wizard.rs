use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use dialoguer::{theme::ColorfulTheme, Input, MultiSelect};
use atty::Stream;

/// ANSI color helpers — produces colored output on TTYs, plain text otherwise.
fn bold_yellow(s: &str) -> String { format!("\x1b[1;33m{}\x1b[0m", s) }
fn bold_green(s: &str) -> String { format!("\x1b[1;32m{}\x1b[0m", s) }
fn cyan(s: &str) -> String { format!("\x1b[36m{}\x1b[0m", s) }
fn magenta(s: &str) -> String { format!("\x1b[35m{}\x1b[0m", s) }
fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
fn yellow(s: &str) -> String { format!("\x1b[33m{}\x1b[0m", s) }
fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }
fn bold_magenta(s: &str) -> String { format!("\x1b[1;35m{}\x1b[0m", s) }

/// A SplashService parsed from agent JSON analysis.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct SplashService {
    pub name: String,
    pub desc: String,
    pub icon: String,
    pub protocol: String,
    pub port: Option<String>,
    pub host_override: Option<String>,
    pub base_path: Option<String>,
    pub group: String,
    #[serde(default)]
    pub web_probe_url: Option<String>,
}

/// A GUI addon pairing (e.g., Ollama -> Ollama Dashboard).
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct GuiAddon {
    pub name: String,
    pub src_port: u16,
    pub icon: String,
}

/// Wizard state produced after user answers all prompts.
pub(crate) struct WizardOutput {
    pub output_dir: PathBuf,
    pub hostname: String,
    pub selected_services: Vec<SplashService>,
    pub gui_addons: Vec<GuiAddon>,
    pub glances_api_base: Option<String>,
}

/// Run the interactive wizard sequence.
pub(crate) fn run(config: &crate::config::SplashConfig) -> anyhow::Result<WizardOutput> {
    let has_tty = atty::is(Stream::Stdin);

    // 1. Determine hostname
    let hostname = config.hostname.clone()
        .or_else(get_hostname)
        .unwrap_or_else(|| "localhost".to_string());
    eprintln!("{}: {}", bold_cyan("Hostname"), cyan(&hostname));

    // 2. Choose agent endpoint and model
    let base_url = ask_agent_endpoint(&config.agent_url)?;
    let models = crate::agent::get_models(&base_url).ok().unwrap_or_default();

    // Let user pick a model from the list (default to first if only one)
    let selected_model_name: String = match models.len() {
        0 => "default".into(),
        1 => models[0].clone(),
        _ => {
            eprintln!("\n{}\n{}", bold_yellow("Available models"), green("---"));
            for (i, m) in models.iter().enumerate() {
                let tag = if i == 0 { " *(default)" } else { "" };
                eprintln!("  [{}]{} {}", cyan(&format!("{}", i)), magenta(m), yellow(tag));
            }
            eprintln!("{}", green("---"));

            let pick: String = Input::new()
                .with_prompt("Select model number (press Enter for first)")
                .default(("0").to_string())
                .interact_text()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            // Resolve numeric selection to actual model name; otherwise use as custom name
            if let Ok(idx) = pick.parse::<usize>() {
                models.get(idx).cloned().unwrap_or(pick)
            } else {
                pick
            }
        }
    };

    let selected_model = &selected_model_name;

    // 3. Gather service probes and send to agent for analysis
    eprintln!("\n{}...", bold_cyan("Gathering host information"));
    let commands = crate::probe::gather_probes();

    let mut all_output: Vec<String> = Vec::with_capacity(commands.len());
    for (i, probe) in commands.iter().enumerate() {
        print!("[{}/{}] {}...", i + 1, commands.len(), bold_yellow(&probe.title));
        std::io::stdout().flush().ok();

        let out = probe.execute();
        all_output.push(out.clone());
        eprintln!(" {}", green("ok"));
    }

    let mut full_input = hostname.to_string();
    for out in &all_output {
        full_input.push('\n');
        full_input.push_str(out);
    }

    // 4. Send to agent for analysis
    eprintln!("\n{}", bold_cyan("Sending to agent for analysis..."));

    // Spawn a background thread to animate a spinner while blocked on HTTP
    let stop_spin = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = std::sync::Arc::clone(&stop_spin);
        std::thread::spawn(move || {
            let frames = ['-', '/', '|', '\\'];
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for &frame in &frames {
                    print!("\r{:<60}{}", " ", frame);
                    std::io::stdout().flush().ok();
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }
        });
    }

    let system_prompt = r#"You are a Linux system analyst. I will give you raw output from system commands (systemctl list-units, docker ps, free, nvidia-smi, etc.).

Return a JSON array of services suitable for a server dashboard page. Only include services humans would care about — skip internal/infrastructure noise, skip the agent itself.

Each entry must have exactly these fields:
- "name": human-readable name (e.g., "Ollama", "Apache2")
- "desc": brief description (5-12 words)
- "icon": one emoji suited to the service
- "protocol": http, https, ssh, vnc, mqtt, ws, etc.
- "port": public port number as string, or null if daemon-only
- "host_override": optional explicit hostname string (null to use input hostname)
- "base_path": optional URL path prefix (null for root)
- "group": group header name — choose from: ["Host Services", "Remote & Guest VMs", "Monitoring", "Development"]
- "web_probe_url": URL for HTTP health-check probe (null if not applicable)

Rules:
1. Only include services with visible UIs, web endpoints, SSH, VNC, MQTT, etc.
2. Daemon-only services without UI get port=null and a note about their purpose
3. Do NOT invent ports, protocols, or hostnames
4. Include GPU info if hardware detection found GPUs and monitoring exists
5. Be concise — 8-16 services max unless genuinely more interesting ones exist

Return ONLY valid JSON. No markdown fences, no explanation."#;

    // Increase max_tokens so large probe outputs don't overflow context window
    const MAX_TOKENS: i32 = 8192;

    let prompt_input = format!("{}\n\n{}", hostname, all_output.join("\n"));

    let analysis_text = {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let resp = client
            .post(format!("{base_url}/v1/chat/completions"))
            .json(&serde_json::json!({
                "model": selected_model,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": prompt_input}
                ],
                "temperature": 0.2,
                "max_tokens": MAX_TOKENS,
            }))
            .send()?;

        // Always read raw text first — don't rely on .json() directly
        let body = resp.text()?;

        if body.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "Agent returned an empty response. This usually means the input was too large for the model's context window. Try running fewer probe commands."
            ));
        }

        let parsed: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!(
                "Agent returned invalid JSON (first 200 chars): {}\nRaw: {}",
                e,
                &body[..body.len().min(200)]
            ))?;

        let content = parsed["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!(
                "Agent returned no message.content. Full response:\n{}",
                &body[..body.len().min(1000)]
            ))?;

        if content.trim().is_empty() {
            return Err(anyhow::anyhow!("Agent returned empty message content.\nFull response:\n{}", &body));
        }

        content.to_string()
    };

    // Stop spinner before parsing (blocking parse)
    stop_spin.store(true, std::sync::atomic::Ordering::Relaxed);

    let all_services = parse_agent_json(&analysis_text)?;

    if all_services.is_empty() {
        anyhow::bail!(bold_red("Agent returned no services to display."));
    }

    eprintln!("\n{}: {}", bold_yellow("Found"), green(&format!("{} potential service(s)", all_services.len())));

    // Group and display for selection
    let mut grouped: HashMap<String, Vec<&SplashService>> = HashMap::new();
    for svc in &all_services {
        grouped.entry(svc.group.clone()).or_default().push(svc);
    }

    let mut selection_flat: Vec<SplashService> = Vec::with_capacity(all_services.len());
    for svc in &all_services {
        selection_flat.push(svc.clone());
    }

    // Group headers for display context
    let mut group_order: Vec<String> = Vec::new();
    for (group_name, _svcs) in grouped.iter() {
        if !group_order.contains(group_name) {
            group_order.push(group_name.clone());
        }
    }

    // Show groups and services with colors
    let theme = ColorfulTheme::default();
    eprintln!("\n{}", bold_magenta("─── Services ───"));
    for gname in &group_order {
        let indices: Vec<usize> = selection_flat.iter().enumerate()
            .filter(|(_i, svc)| svc.group == *gname)
            .map(|(i, _)| i)
            .collect();

        eprintln!("\n{} {} ({}):", bold_green("▸"), yellow(gname), indices.len());
        for idx in &indices {
            let svc = &selection_flat[*idx];
            let port_display = svc.port.as_deref().unwrap_or("daemon");
            eprintln!(
                "  {}",
                format!(
                    "[{}] {} {} — {} (port: {}) — {}",
                    *idx,
                    magenta(&svc.icon),
                    green(&svc.name),
                    svc.desc.chars().take(40).collect::<String>(),
                    cyan(port_display),
                    svc.protocol
                )
            );
        }
    }

    let mut checked_indices: Vec<usize> = Vec::new();
    if !selection_flat.is_empty() {
        if has_tty {
            let options: Vec<String> = selection_flat.iter().enumerate().map(|(i, s)| {
                let port_str = s.port.as_deref().unwrap_or("daemon");
                format!("[{}] {} {} (port: {})", i, s.icon, s.name, port_str)
            }).collect();

            eprintln!("\n{}", bold_magenta("─── Select services ───"));
            eprintln!("{}", green("space=toggle, enter=confirm"));
            for opt in &options {
                eprintln!("  {}", opt);
            }

            let selected = MultiSelect::with_theme(&theme)
                .items(&options)
                .defaults(&vec![true; selection_flat.len()][..])
                .interact()
                .unwrap_or_default();

            checked_indices = selected.to_vec();
        } else {
            eprintln!("\n(Non-TTY detected — selecting all {} service(s) by default)", selection_flat.len());
            checked_indices = (0..selection_flat.len()).collect();
        }
    }

    let selected: Vec<SplashService> = checked_indices.iter()
        .filter_map(|&i| selection_flat.get(i).cloned())
        .collect();

    if selected.is_empty() {
        eprintln!("{}", green("No services selected. Generating splash page without services."));
    } else {
        eprintln!("\n{}: {}", bold_green("Selected"), red(&format!("{} service(s)", selected.len())));
    }

    // 6. Gather and display known GUI pairings
    let gui_file = config.gui_pairs_file.as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir().unwrap_or_default()
                .join("server-splash/services.toml")
        });
    let gui_addons: Vec<GuiAddon> = load_gui_pairs(&gui_file);

    // Determine output directory
    let user_input_dir = config.get_output_dir("./server-splash");

    // Show GUI options
    let mut selected_gui: Vec<GuiAddon> = Vec::new();
    if !gui_addons.is_empty() {
        let gui_options: Vec<String> = gui_addons.iter().map(|g| {
            format!("{} {} -> :{}", g.icon, g.name, g.src_port)
        }).collect();

        eprintln!("\n{}", bold_magenta("─── Known GUI addons ───"));
        eprintln!("{}", green("these will be installed and linked in the splash page"));
        for opt in &gui_options {
            eprintln!("  {}", opt);
        }

        if has_tty {
            let selected = MultiSelect::with_theme(&theme)
                .items(&gui_options)
                .interact()
                .unwrap_or_default();
            selected_gui = selected.iter().filter_map(|&i| gui_addons.get(i).cloned()).collect();
        } else if std::env::var("SPLASH_AUTO_INSTALL_GUI").ok().as_deref() == Some("1") {
            selected_gui = gui_addons.clone();
        } else {
            eprintln!("\n(Non-TTY detected — no GUI addons selected. Set SPLASH_AUTO_INSTALL_GUI=1 to include all)");
        }
    };

    eprintln!("\n{}: {}", bold_yellow("Splash page location"), green(&user_input_dir));

    // 7. Persist config if hostname/agent is set
    let cfg_persist = crate::config::SplashConfig {
        agent_url: base_url,
        hostname: Some(hostname.clone()),
        output_dir: Some(user_input_dir.clone()),
        gui_pairs_file: Some(gui_file.to_string_lossy().into_owned()),
        glances_api_base: config.glances_api_base.clone(),
    };
    cfg_persist.save_config();

    Ok(WizardOutput {
        output_dir: PathBuf::from(user_input_dir),
        hostname,
        selected_services: selected,
        gui_addons: selected_gui,
        glances_api_base: config.glances_api_base.clone(),
    })
}

fn bold_cyan(s: &str) -> String { format!("\x1b[1;36m{}\x1b[0m", s) }
fn bold_red(s: &str) -> String { format!("\x1b[1;31m{}\x1b[0m", s) }

fn get_hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().or_else(|| {
        let p = std::process::Command::new("hostname")
            .output()
            .ok()?;
        if !p.status.success() { return None; }
        String::from_utf8(p.stdout).ok().map(|s| s.trim().to_string())
    })
}

fn ask_agent_endpoint(default_url: &str) -> anyhow::Result<String> {
    let found = crate::agent::probe_agent_endpoints();

    if !found.is_empty() && found.len() != 1 {
        eprintln!("\n{} {} OpenAI-compatible endpoint(s):", bold_yellow("Found"), green(&format!("{}", found.len())));
        for (i, url) in found.iter().enumerate() {
            eprintln!("  [{}] {}", i, cyan(url));
        }
        eprintln!("  [0] Enter custom URL");
    }

    let prompt = if !found.is_empty() {
        format!("Choose endpoint (default {}):", found[0])
    } else {
        "Enter agent API URL (e.g. http://127.0.0.1:11434)".to_string()
    };

    Input::new()
        .with_prompt(prompt)
        .default(found.first().cloned().unwrap_or_else(|| default_url.to_string()))
        .show_default(!found.is_empty())
        .interact_text()
        .map(|s| {
            if s.trim().is_empty() && !found.is_empty() {
                found[0].clone()
            } else {
                s.trim().to_string()
            }
        })
        .map_err(|e| anyhow::anyhow!("Failed to read URL: {}", e))
}

/// Try to recover a complete Vec<SplashService> from truncated/invalid JSON.
fn recover_services_from_text(raw: &str) -> Option<Vec<serde_json::Value>> {
    let trimmed = raw.trim();

    // If it looks like an array, collect every top-level complete object by depth scanning.
    if trimmed.starts_with('[') {
        let bytes = trimmed.as_bytes();
        let len = bytes.len();
        let mut objects: Vec<&str> = Vec::new();
        let mut i = 0;

        while i < len {
            if bytes[i] == b'{' {
                // Scan to find matching closing brace, respecting strings.
                let mut depth: i32 = 0;
                let mut in_str = false;
                let mut esc = false;
                let mut j = i;
                while j < len {
                    if in_str {
                        if esc { esc = false; j += 1; continue; }
                        if bytes[j] == b'"' { in_str = false; }
                        j += 1;
                        continue;
                    }
                    match bytes[j] {
                        b'"' => in_str = true,
                        b'\\' => esc = true,
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                // Complete object found. Check if it looks like a JSON object (has ':')
                                let chunk = &raw.trim()[i..=j];
                                if chunk.contains(':') {
                                    objects.push(chunk);
                                }
                                i = j + 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                // If we hit the end without finding a closing brace, stop.
                if depth > 0 { break; }
            } else {
                i += 1;
            }
        }

        if !objects.is_empty() {
            let join_str = format!("[{}]", objects.join(", "));
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&join_str) {
                return Some(arr);
            }
        }
    }

    None
}

fn parse_agent_json(text: &str) -> anyhow::Result<Vec<SplashService>> {
    let text = text.trim();

    // Strip markdown fences
    let stripped = if let Some(rest) = text.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = text.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        text
    };

    // Try direct parse (with smart-quote normalization fallback)
    let try_parse = |raw: &str| -> Option<Vec<SplashService>> {
        if let Ok(svcs) = serde_json::from_str::<Vec<SplashService>>(raw) { return Some(svcs); }
        let norm: String = raw.chars().map(|c| match c {
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{2014}' | '\u{2013}' => '-',
            _ => c,
        }).collect();
        if let Ok(svcs) = serde_json::from_str::<Vec<SplashService>>(&norm) { return Some(svcs); }
        None
    };

    if let Some(svcs) = try_parse(stripped) { return Ok(svcs); }

    // --- Truncated JSON recovery: extract complete objects from the array ---
    // When the stream is cut off mid-key ("web"), we scan for '{ at brace_depth=0
    // (top-level objects inside a '[' array) and collect all complete ones.

    let maybe_recovered = recover_services_from_text(stripped);
    if let Some(arr) = maybe_recovered {
        return parse_services_from_arr(&arr);
    }

    let maybe_json = stripped.trim().strip_prefix('{')
        .map(|s| format!("{{{}", s))
        .unwrap_or_else(|| format!("[{}]", stripped));

    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&maybe_json) {
        for key in &["result", "services"] {
            if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
                return parse_services_from_arr(arr);
            }
        }
    }

    // Last resort: try parsing individual objects from within the raw text
    for key in &["result", "services"] {
        let pat = format!("\"{}\"", key);
        if let Some(idx) = stripped.find(&pat) {
            let rest_no = &stripped[idx..];
            if let Some(after_colon) = rest_no.find(':') {
                let maybe_arr = &rest_no[after_colon + 1..];
                let start = maybe_arr.find('[').map(|i| i).unwrap_or(0);
                let end_idx = maybe_arr.find(']').map(|i| i + 1).unwrap_or(maybe_arr.len());
                if let Some(arr_str) = maybe_arr.get(start..end_idx) {
                    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(arr_str) {
                        return parse_services_from_arr(&arr);
                    }
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "Failed to parse agent analysis as JSON. Response received:\n---\n{}\n---\n\nPlease ensure the agent returns a valid JSON array of service objects.",
        &stripped[..(stripped.len().min(500))]
    ))
}

fn parse_services_from_arr(arr: &[serde_json::Value]) -> anyhow::Result<Vec<SplashService>> {
    Ok(arr.iter().map(|v| SplashService {
        name: v["name"].as_str().unwrap_or("").to_string(),
        desc: v["desc"].as_str().unwrap_or("").to_string(),
        icon: v["icon"].as_str().unwrap_or("🛠️").to_string(),
        protocol: v.get("protocol")
            .or_else(|| v.get("proto"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default(),
        port: v.get("port").and_then(|v| v.as_str()).map(String::from),
        host_override: v.get("host_override") // handle both underscores and hyphens
            .or_else(|| v.get("host-override"))
            .and_then(|v| v.as_str())
            .map(String::from),
        base_path: v.get("base_path") // handle both underscores and hyphens
            .or_else(|| v.get("basePath"))
            .and_then(|v| v.as_str())
            .map(String::from),
        group: v["group"].as_str().unwrap_or("Host Services").to_string(),
        web_probe_url: v.get("web_probe_url") // handle both underscores and hyphens
            .or_else(|| v.get("web-probe-url"))
            .and_then(|v| v.as_str())
            .map(String::from),
    }).collect())
}

/// Load predefined GUI pairings from a TOML file at the configured path.
fn load_gui_pairs(path: &Path) -> Vec<GuiAddon> {
    if !path.exists() {
        return default_gui_addons();
    }

    let content = std::fs::read_to_string(path);
    if let Ok(content) = content {
        if let Ok(table) = content.parse::<toml::Table>() {
            let mut addons: Vec<GuiAddon> = Vec::new();
            for (name, entry) in &table {
                if let Some(table2) = entry.as_table() {
                    let port = table2.get("src_port")
                        .or_else(|| table2.get("src-port"))
                        .and_then(|v| v.as_integer())
                        .unwrap_or(0) as u16;
                    let icon = table2.get("icon")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| "🛠️".to_string());
                    addons.push(GuiAddon {
                        name: name.clone(),
                        src_port: port,
                        icon,
                    });
                }
            }
            if !addons.is_empty() {
                return addons;
            }
        }
    }

    default_gui_addons()
}

/// Default GUI pairings — shipped if no custom file exists.
fn default_gui_addons() -> Vec<GuiAddon> {
    vec![
        GuiAddon { name: "Ollama Dashboard".to_string(), src_port: 11434, icon: "🤖".to_string() },
        GuiAddon { name: "Dockge (Docker Manager)".to_string(), src_port: 5001, icon: "🐳".to_string() },
        GuiAddon { name: "Traefik Dashboard".to_string(), src_port: 8080, icon: "🔀".to_string() },
        GuiAddon { name: "Portainer Container UI".to_string(), src_port: 9000, icon: "🖥️".to_string() },
        GuiAddon { name: "Grafana Dashboard".to_string(), src_port: 3001, icon: "📊".to_string() },
        GuiAddon { name: "Glances Monitoring".to_string(), src_port: 6120, icon: "📈".to_string() },
    ]
}
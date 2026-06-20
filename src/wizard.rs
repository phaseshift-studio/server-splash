use std::io::Write;
use std::path::{Path, PathBuf};
use dialoguer::Input;
use atty::Stream;
use unicode_width::UnicodeWidthChar;

/// ANSI color helpers — produces colored output on TTYs, plain text otherwise.
fn bold_yellow(s: &str) -> String { format!("\x1b[1;33m{}\x1b[0m", s) }
fn bold_green(s: &str) -> String { format!("\x1b[1;32m{}\x1b[0m", s) }
fn cyan(s: &str) -> String { format!("\x1b[36m{}\x1b[0m", s) }
fn magenta(s: &str) -> String { format!("\x1b[35m{}\x1b[0m", s) }
fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
fn yellow(s: &str) -> String { format!("\x1b[33m{}\x1b[0m", s) }
fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }

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

// ─── TUI drawing helpers ──────────────────────────────────────────────

fn display_w(s: &str) -> usize {
    s.chars().fold(0, |n, c| if c == '\x1b' { n } else { n + c.width().unwrap_or(0) })
}

fn strip_esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut esc = false;
    for c in s.chars() {
        if c == '\x1b' { esc = true; }
        else if esc { if c == 'm' { esc = false; } }
        else { out.push(c); }
    }
    out
}

/// Draw a boxed menu with title and items. Items may contain ANSI colors.
fn box_menu(title: &str, items: &[String]) -> String {
    // Hostname
    let config = crate::config::SplashConfig::load();
    let hostname = config.hostname
        .clone()
        .or_else(get_hostname)
        .unwrap_or_else(|| "localhost".to_string());
    eprintln!("{}: {}", bold_cyan("Hostname"), cyan(&hostname));

    if items.is_empty() { return String::new(); }

    let mut max_w = display_w(title) as i32;
    for it in items {
        let w = display_w(it) as i32;
        if w > max_w { max_w = w; }
    }
    max_w = (max_w + 6).min(78);
    let w = max_w.max(24);

    fn pad(n: usize) -> String { " ".repeat(n) }
    let sep = "─".repeat(w as usize - 2);

    let mut o = String::new();

    // Top border (yellow)
    o.push_str(&yellow(&format!("┌{}┐", sep)));
    o.push('\n');

    // Title centered
    let lw = ((w - 4) - display_w(title).max(1) as i32).max(0) / 2;
    o.push_str("│ ");
    o.push_str(&pad(lw as usize));
    o.push_str(&bold_title(title));
    let rw = (w - 4 - lw - display_w(title).max(1) as i32).max(0);
    o.push_str(&pad(rw as usize));
    o.push_str(" │\n");

    // Separator under title
    o.push_str(&format!("├{}┤", "─".repeat(w as usize - 2)));
    o.push('\n');

    // Items with indentation
    for it in items {
        let bare = strip_esc(it);
        let iw = display_w(&bare) as i32;
        // Pad the line so that total visible width equals `w`. The item string already
        // includes its own ANSI colour codes, which `display_w` ignores. We add a 3‑char
        // left indent and a 2‑char right padding (space + vertical bar). Thus the
        // remaining space is `w - iw - 6`.
        o.push_str("│   ");                    // indent inside box
        o.push_str(it);
        let rem = (w - iw - 6).max(0);
        o.push_str(&pad(rem as usize));
        // Revert original spacing – one more space was added earlier. The
        // correct string is three spaces followed by the vertical bar.
        o.push_str(" │\n");
    }

    o.push_str(&yellow(&format!("└{}┘", sep)));
    o
}


fn bold_title(s: &str) -> String { format!("\x1b[1m{}\x1b[0m", s) }

/// Result of probing a single service endpoint.
#[derive(Debug, Clone)]
pub(crate) struct ProbeResult {
    pub name: String,
    pub alive: bool,
    pub icon: String,
    pub endpoint: String,
    pub description: String,
    pub selected: bool,
}

/// Persistable session state — saved as splash-state.json in the output directory.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct SplashState {
    hostname: String,
    services: Vec<SplashService>,
    selected_modules: Vec<String>,
    glances_api_base: Option<String>,
}

/// Wizard state produced after user answers all prompts.
pub(crate) struct WizardOutput {
    pub output_dir: PathBuf,
    pub hostname: String,
    pub selected_services: Vec<SplashService>,
    pub _probe_results: Vec<ProbeResult>,
    pub selected_modules: Vec<String>,
    pub glances_api_base: Option<String>,
    pub deploy_path: Option<String>,
}

/// Wrap text into lines of given width. Returns wrapped lines.
fn wrap_lines(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        if len <= cols {
            out.push(line.to_string());
            continue;
        }
        let mut start = 0;
        while start < len {
            // `end` is the half-open upper bound: chars[start..end) gets consumed
            let end = std::cmp::min(start + cols, len);

            // Scan for last whitespace in [start..end), convert to absolute index.
            let break_pos_opt: Option<usize> = chars[start..end]
                .iter()
                .rposition(|c| c.is_whitespace())
                .map(|i| start + i);

            let break_idx = match break_pos_opt {
                Some(p) if p > start => p - 1,  // break before the whitespace
                _ => end.checked_sub(1).unwrap_or(start), // force word-break at boundary
            };

            let break_at = std::cmp::max(break_idx, start).min(len.saturating_sub(1));
            out.push(chars[start..=break_at].iter().collect());
            start = break_at + 1;
        }
    }
    out
}

/// Draw a grid box around given lines with a title row.
fn draw_grid(title: &str, lines: &[&str], max_lines: usize) -> String {
    // Strip ANSI codes first — wrap plain text only (escape sequences inflate width calc and cause mid-line wraps)
    let plain = lines.iter().map(|l| strip_esc(l)).collect::<Vec<_>>();
    let display_lines = wrap_lines(&plain.join("\n"), 52);

    let count = (max_lines * 4).min(display_lines.len());
    let clipped = &display_lines[..count];

    if clipped.is_empty() {
        return format!("[ {} ] — no data", title);
    }

    let more_count = display_lines.len().saturating_sub(count);
    let has_more = more_count > 0;
    let show_lines: Vec<String> = if has_more {
        clipped[..count - 1].to_vec()
    } else {
        clipped.to_vec()
    };

    // Determine column width from plain text content, cap at max terminal width
    let mut w = title.len() as i32 + 4;
    for l in &show_lines { w = std::cmp::max(w, (l.len() + 4) as i32); }
    if has_more { w = std::cmp::max(w, ("{} more lines".len() + 5) as i32); }
    w = w.clamp(10, 80);

    fn cell(pad: usize) -> String { " ".repeat(pad) }

    let mut out = String::new();

    // Title line (centered with emoji)
    out.push_str(&bold_yellow(&format!("📌 [ {} ]", title)));
    out.push('\n');

    // Content lines (plain text — ANSI codes caused width inflation)
    for line in &show_lines {
        let iw = w as usize;
        if line.len() >= iw - 4 {
            out.push_str("│ ");
            out.push_str(&line[..(iw - 4)]);
            out.push_str(" │");
        } else {
            out.push_str("│ ");
            out.push_str(line);
            let pad_w = (iw as i32 - 4 - line.len() as i32).max(0) as usize;
            out.push_str(&cell(pad_w));
            out.push('│');
        }
        out.push('\n');
    }

    // More lines indicator
    if has_more {
        let msg = format!("{} more lines", bold_yellow(&more_count.to_string()));
        if display_w(&msg) as i32 >= w - 4 {
            out.push_str("│ ");
            let bare = strip_esc(&msg);
            out.push_str(&bare.chars().take((w as usize) - 4).collect::<String>());
            out.push('\n');
        } else {
            let pad_w = (w - 4 - display_w(&msg) as i32).max(0) as usize;
            out.push_str("│ ");
            out.push_str(&msg);
            if pad_w > 0 {
                out.push_str(&cell(pad_w));
            }
            out.push_str("│\n");
        }
        out.push('\n');
    }

    // Bottom border (plain ASCII, no ANSI)
    out.push_str(&format!("└{}┘", "─".repeat((w - 2) as usize)));

    out
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

    // Verify the endpoint is reachable before proceeding
    if !crate::agent::is_openai_compatible(&base_url) {
        anyhow::bail!(format!("{}: Agent endpoint {} is unreachable or not responding. Check the URL and try again.",
            bold_red("Error"), base_url));
    }

    // Verify it actually returns models
    let models = match crate::agent::get_models(&base_url) {
        Ok(m) if !m.is_empty() => m,
        _ => anyhow::bail!(format!("{}: Agent endpoint {} returned no available models. Select a different endpoint or check Ollama.",
            bold_red("Error"), base_url)),
    };

    // Try to find the currently loaded model and use it as default
    let current_model = crate::agent::get_default_model(&base_url);
    let default_idx = current_model
        .as_ref()
        .and_then(|cm| models.iter().position(|m| m == cm))
        .unwrap_or(0);

    // Let user pick a model from the list (default to currently loaded, or first)
    let selected_model_name: String = match models.len() {
        0 => "default".into(),
        1 => models[0].clone(),
        _ => {
            // Model picker — boxed menu
            let mut model_lines = Vec::with_capacity(models.len());
            for (i, m) in models.iter().enumerate() {
                let tag = if i == default_idx { " *(default)" } else { "" };
                model_lines.push(format!("{} {}{}\x1b[K", cyan(&format!("[{i}]")), magenta(m), yellow(tag)));
            }
            eprintln!("{}", box_menu("Select Model", &model_lines));

            let pick: String = Input::new()
                .with_prompt("Select model number (press Enter for default)")
                .default(default_idx.to_string())
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

    // Build the prompt content (hostname + system_prompt + raw output) that will be sent to agent.
    // Only construct this once just before use so we don't hold a giant string in memory needlessly.

    // 4. Determine output directory early (used for cache + final output)
    let user_input_dir = config.get_output_dir("./server-splash");
    let cache_path = PathBuf::from(&user_input_dir).join("agent-response.json");
    let state_path = PathBuf::from(&user_input_dir).join("splash-state.json");

    // Check if a previous session exists and offer to reload it
    if state_path.exists() && has_tty {
        eprintln!("\n{} {}",
            bold_cyan("Previous session found:"),
            cyan(&state_path.to_string_lossy()));
        let pick: String = Input::new()
            .with_prompt("Load previous session? [Y/n]")
            .default("y".to_string())
            .interact_text()
            .unwrap_or_default();
        if pick.trim().to_lowercase() != "n" {
            let loaded: SplashState = serde_json::from_str(
                &std::fs::read_to_string(&state_path)
                    .map_err(|e| anyhow::anyhow!("Failed to read state: {e}"))?
            )?;
            eprintln!("{} {} services, {} modules",
                bold_yellow("Loaded"),
                loaded.services.len(),
                loaded.selected_modules.len());

            let mut services = loaded.services.clone();
            let mut state = loaded.clone();
            let probe_results = probe_and_display_services(
                &mut services, &loaded.hostname, has_tty, &mut state, &state_path,
            );

            // Filter deselected services
            let keep_names: std::collections::HashSet<&str> = probe_results.iter()
                .map(|r| r.name.as_str()).collect();
            services.retain(|s| keep_names.contains(s.name.as_str()));

            // Save final state after edits
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                let _ = std::fs::write(&state_path, json);
            }

            return Ok(WizardOutput {
                output_dir: PathBuf::from(user_input_dir),
                hostname: state.hostname.clone(),
                selected_services: services,
                _probe_results: probe_results,
                selected_modules: state.selected_modules,
                glances_api_base: state.glances_api_base,
                deploy_path: None,
            });
        }
    }

    let use_cache = if cache_path.exists() {
        if has_tty {
            let cached_len = std::fs::metadata(&cache_path)
                .map(|m| m.len())
                .unwrap_or(0);
            eprintln!("\n{} {} ({} bytes)",
                bold_cyan("Cached agent response found:"),
                cyan(&cache_path.to_string_lossy()),
                cached_len);
            let pick: String = Input::new()
                .with_prompt("Use cached response? [Y/n]")
                .default("y".to_string())
                .interact_text()
                .unwrap_or_default();
            pick.trim().to_lowercase() != "n"
        } else {
            true // non-TTY: always use cache
        }
    } else {
        false
    };

    let analysis_text: String = if use_cache {
        let cached = std::fs::read_to_string(&cache_path)
            .map_err(|e| anyhow::anyhow!("Failed to read cached agent response: {e}"))?;
        eprintln!("{} ({} chars)",
            bold_yellow("Using cached response"),
            cached.len());
        cached
    } else {
        eprintln!("\n{}", bold_cyan("Sending to agent for analysis..."));

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
        eprintln!();
        show_agent_prompt_box(system_prompt);

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

        let prompt_content = format!("{}\n\n{}", hostname, all_output.join("\n"));

        // Increase max_tokens so large probe outputs don't overflow context window
        const MAX_TOKENS: i32 = 8192;

        let text = {
            // Re-verify endpoint is still alive — probes took time, Ollama may have unloaded the model
            if !crate::agent::is_openai_compatible(&base_url) {
                anyhow::bail!(format!("{}: Agent endpoint {} is no longer reachable after probe collection.",
                    bold_red("Error"), base_url));
            }

            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?;

            let resp = client
                .post(format!("{base_url}/v1/chat/completions"))
                .json(&serde_json::json!({
                    "model": selected_model,
                    "messages": [
                        {"role": "system", "content": system_prompt},
                        {"role": "user", "content": prompt_content}
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

        // Stop spinner before saving/parsing
        stop_spin.store(true, std::sync::atomic::Ordering::Relaxed);

        // Save to cache
        std::fs::create_dir_all(&user_input_dir)
            .map_err(|e| anyhow::anyhow!("Failed to create output dir: {e}"))?;
        std::fs::write(&cache_path, &text)
            .map_err(|e| anyhow::anyhow!("Failed to cache agent response: {e}"))?;

        text
    };

    eprintln!("\n{} {}", bold_yellow("agent analysis"), cyan("(ctrl-o to expand)"));

    // Show the agent's raw response as a compact summary grid.
    let total_chars = analysis_text.len();
    let lines: Vec<&str> = analysis_text.lines().collect();
    eprintln!("\n{}", yellow(&draw_grid("Agent Response", &lines, 8)));
    eprintln!("  Total: {} chars", cyan(&format!("{}", total_chars)));

    let mut all_services = parse_agent_json(&analysis_text)?;

    // Deduplicate by normalized name (case-insensitive) to remove agent duplicates
    let mut seen_names = std::collections::HashSet::new();
    all_services.retain(|svc| {
        let key = svc.name.to_lowercase();
        seen_names.insert(key) // keep = true (first), remove = false (duplicate)
    });

    if all_services.is_empty() {
        anyhow::bail!(bold_red("Agent returned no services to display."));
    }

    eprintln!("\n{}: {}", bold_yellow("Found"), green(&format!("{} potential service(s)", all_services.len())));

    // All services selected by default; deselection happens in the probe table via 's' key
    let mut selected: Vec<SplashService> = all_services;

    eprintln!("\n{}: {} (toggle in probe table with 's' key)",
        bold_green("All services selected"), red(&format!("{} service(s)", selected.len())));

    // 6. Gather and display known GUI pairings
    let gui_file = config.gui_pairs_file.as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir().unwrap_or_default()
                .join("server-splash/services.toml")
        });
    let gui_addons: Vec<GuiAddon> = load_gui_pairs(&gui_file);

    // Ask user which GUI addons to include
    let mut gui_selection: Vec<SplashService> = Vec::new();
    if !gui_addons.is_empty() && has_tty {
        let mut gui_lines: Vec<String> = gui_addons.iter().enumerate().map(|(i, g)| {
            format!("[{}] {} {} — :{}", cyan(&format!("{}", i)), green(&g.icon), yellow(&g.name), green(&format!("{}", g.src_port)))
        }).collect();

        gui_lines.push(String::new());
        gui_lines.push("  use comma-separated indices like 0,2-4".to_string());
        gui_lines.push("  press Enter to select all".to_string());
        eprintln!("{}", box_menu("Select GUI Addons (optional)", &gui_lines));

        let picked: String = Input::new()
            .with_prompt("Select GUI addon indices")
            .default(String::from(""))
            .interact_text()
            .unwrap_or_default();

        if picked.is_empty() {
            // Select all
            for g in gui_addons {
                gui_selection.push(create_gui_service(&g, &hostname));
            }
        } else {
            let mut indices: Vec<usize> = Vec::new();
            for part in picked.split(',') {
                let part = part.trim();
                if let Some((lo_str, hi_str)) = part.split_once('-') {
                    if let (Ok(lo), Ok(hi)) = (lo_str.parse::<usize>(), hi_str.parse::<usize>()) {
                        for idx in lo..=hi.min(gui_addons.len() - 1) {
                            if !indices.contains(&idx) {
                                indices.push(idx);
                            }
                        }
                    }
                } else if let Ok(idx) = part.parse::<usize>() {
                    if idx < gui_addons.len() && !indices.contains(&idx) {
                        indices.push(idx);
                    }
                }
            }
            for &idx in &indices {
                if let Some(g) = gui_addons.get(idx) {
                    gui_selection.push(create_gui_service(g, &hostname));
                }
            }
        }
    }

    eprintln!("\n{}: {}", bold_yellow("Splash page location"), green(&user_input_dir));

    // 5b. Discover and offer dashboard modules from src/modules/
    let available_modules = crate::modules::all_modules();
    let mut selected_module_names: Vec<String> = Vec::new();
    if !available_modules.is_empty() && has_tty {
        let mut mod_lines: Vec<String> = available_modules.iter().enumerate().map(|(i, m)| {
            format!("[{}] {} {} — :{}  {}", cyan(&format!("{}", i)), green(&m.icon), yellow(&m.name), green(&format!("{}", m.default_port)), m.description)
        }).collect();
        mod_lines.push(String::new());
        mod_lines.push("  use comma-separated indices like 0,2-4".to_string());
        mod_lines.push("  press Enter to select all".to_string());
        eprintln!("{}", box_menu("Select Dashboard Modules (optional)", &mod_lines));

        let picked: String = Input::new()
            .with_prompt("Select module indices")
            .default(String::from(""))
            .interact_text()
            .unwrap_or_default();

        if picked.is_empty() {
            for m in &available_modules {
                selected_module_names.push(m.name.clone());
                selected.push(SplashService {
                    name: m.name.clone(),
                    desc: m.description.clone(),
                    icon: m.icon.clone(),
                    protocol: "http".to_string(),
                    port: Some(m.default_port.to_string()),
                    host_override: None,
                    base_path: Some(format!("/{}", m.url_prefix.trim_start_matches('/'))),
                    group: "Dashboard Modules".to_string(),
                    web_probe_url: None,
                });
            }
        } else {
            let mut indices: Vec<usize> = Vec::new();
            for part in picked.split(',') {
                let part = part.trim();
                if let Some((lo_str, hi_str)) = part.split_once('-') {
                    if let (Ok(lo), Ok(hi)) = (lo_str.parse::<usize>(), hi_str.parse::<usize>()) {
                        for idx in lo..=hi.min(available_modules.len() - 1) {
                            if !indices.contains(&idx) {
                                indices.push(idx);
                            }
                        }
                    }
                } else if let Ok(idx) = part.parse::<usize>() {
                    if idx < available_modules.len() && !indices.contains(&idx) {
                        indices.push(idx);
                    }
                }
            }
            for &idx in &indices {
                if let Some(m) = available_modules.get(idx) {
                    selected_module_names.push(m.name.clone());
                    selected.push(SplashService {
                        name: m.name.clone(),
                        desc: m.description.clone(),
                        icon: m.icon.clone(),
                        protocol: "http".to_string(),
                        port: Some(m.default_port.to_string()),
                        host_override: None,
                        base_path: Some(format!("/{}", m.url_prefix.trim_start_matches('/'))),
                        group: "Dashboard Modules".to_string(),
                        web_probe_url: None,
                    });
                }
            }
        }
    }

    // Merge GUI addon services into the final selection
    selected.extend(gui_selection);

    // 6. Probe HTTP endpoints and show status table before generating HTML
    let state_path = PathBuf::from(&user_input_dir).join("splash-state.json");
    let mut state = SplashState {
        hostname: hostname.clone(),
        services: selected.clone(),
        selected_modules: selected_module_names.clone(),
        glances_api_base: None, // filled in below
    };
    let probe_results = probe_and_display_services(
        &mut selected, &hostname, has_tty, &mut state, &state_path,
    );

    // Filter deselected services from the final list
    let deselected_names: std::collections::HashSet<&str> = probe_results.iter()
        .filter(|r| !r.selected)
        .map(|r| r.name.as_str())
        .collect();
    selected.retain(|s| !deselected_names.contains(s.name.as_str()));

    // 7. Persist config if hostname/agent is set
    let cfg_persist = crate::config::SplashConfig {
        agent_url: base_url.clone(),
        hostname: Some(hostname.clone()),
        output_dir: Some(user_input_dir.clone()),
        gui_pairs_file: Some(gui_file.to_string_lossy().into_owned()),
        glances_api_base: config.glances_api_base.clone(),
    };
    cfg_persist.save_config();

    // Determine Glances API base for GPU stats.
    // Priority: config > auto-detect from services list > probe localhost ports
    let glances_from_config = config.glances_api_base.clone();
    let glances_from_services = selected.iter().find(|s| s.name.to_lowercase().contains("glances"))
        .map(|s| format!(
            "http://{}:{}",
            s.host_override.as_deref().unwrap_or(&hostname),
            s.port.as_deref().unwrap_or("61208")
        ));

    let glances_api_base = glances_from_config
        .or(glances_from_services)
        .or_else(|| {
            use reqwest::blocking::Client;
            let client = Client::new();
            for probe_port in [61208u16, 6120] {
                let url = format!("http://127.0.0.1:{probe_port}");
                if let Ok(resp) = client.get(format!("{url}/api/4/gpu")).send() {
                    if resp.status().is_success() {
                        return Some(url);
                    }
                }
            }
            None
        });

    // Save final state after all edits
    state.glances_api_base = glances_api_base.clone();
    state.services = selected.clone();
    state.selected_modules = selected_module_names.clone();
    if let Ok(json) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(&state_path, json);
    }

    // 8. Ask where to deploy
    let deploy_path = if has_tty {
        let www = "/var/www/html";
        let has_www = std::path::Path::new(www).is_dir();

        eprintln!("\n{}", bold_cyan("Deployment"));
        eprintln!("  [0] {} (keep files in place)", cyan(&user_input_dir));
        if has_www {
            eprintln!("  [1] {} (standard web root)", cyan(www));
        }
        let custom_n = if has_www { 2 } else { 1 };
        let skip_n = custom_n + 1;
        eprintln!("  [{}] Custom path...", custom_n);
        eprintln!("  [{}] Skip deployment", skip_n);
        eprintln!();

        let default = String::from("0");
        let pick: String = Input::new()
            .with_prompt("Choose deploy target")
            .default(default)
            .interact_text()
            .unwrap_or_default();

        match pick.trim() {
            "0" => Some(user_input_dir.clone()),
            "1" if has_www => Some(www.to_string()),
            n if n.parse::<usize>().is_ok() => {
                let idx: usize = n.parse().unwrap();
                if idx == custom_n {
                    let custom: String = Input::new()
                        .with_prompt("Enter deploy path")
                        .interact_text()
                        .unwrap_or_default();
                    if custom.is_empty() { None } else { Some(custom) }
                } else if idx == skip_n {
                    None
                } else {
                    Some(user_input_dir.clone())
                }
            }
            _ => Some(user_input_dir.clone()),
        }
    } else {
        None
    };

    Ok(WizardOutput {
        output_dir: PathBuf::from(user_input_dir),
        hostname,
        selected_services: selected,
        _probe_results: probe_results,
        selected_modules: selected_module_names,
        glances_api_base,
        deploy_path,
    })
}

/// Build an HTTP probe URL for a service. Returns None for non-HTTP services.
fn build_probe_url(svc: &SplashService, hostname: &str) -> Option<String> {
    if let Some(ref wp) = svc.web_probe_url {
        if !wp.is_empty() && wp.to_lowercase() != "none" {
            return Some(wp.clone());
        }
    }
    let port = svc.port.as_deref()?;
    let protocol = svc.protocol.to_lowercase();
    if matches!(protocol.as_str(), "ssh" | "vnc" | "mqtt" | "ws") {
        return None;
    }
    let host = svc.host_override.as_deref().unwrap_or(hostname);
    let bp = svc.base_path.as_deref().unwrap_or("");
    Some(format!("http://{}:{}{}", host, port, bp))
}

/// Probe a single HTTP endpoint with a 3-second timeout.
fn probe_http(url: &str) -> bool {
    use reqwest::blocking::Client;
    let client = match Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send() {
        Ok(resp) => resp.status().is_success() || resp.status().is_redirection(),
        Err(_) => false,
    }
}

/// Read a line of input with the given initial text pre-filled for inline editing.
fn read_line_with_initial(prompt: &str, initial: &str) -> String {
    use console::Term;
    let term = Term::stderr();

    let mut buf: Vec<char> = initial.chars().collect();
    let mut pos = buf.len();

    eprint!("  {}: ", prompt);

    // Redraw helper: clear line, print prompt + buffer, position cursor
    let redraw = |buf: &[char], pos: usize| {
        eprint!("\r\x1b[K  {}: {}", prompt, buf.iter().collect::<String>());
        // Move cursor back to pos within the content
        let back = buf.len().saturating_sub(pos);
        if back > 0 {
            eprint!("\x1b[{}D", back);
        }
    };

    redraw(&buf, pos);

    loop {
        match term.read_key() {
            Ok(console::Key::Enter) => {
                eprintln!();
                break;
            }
            Ok(console::Key::Char(c)) => {
                buf.insert(pos, c);
                pos += 1;
                redraw(&buf, pos);
            }
            Ok(console::Key::Backspace) => {
                if pos > 0 {
                    pos -= 1;
                    buf.remove(pos);
                    redraw(&buf, pos);
                }
            }
            Ok(console::Key::ArrowLeft) => {
                if pos > 0 {
                    pos -= 1;
                    redraw(&buf, pos);
                }
            }
            Ok(console::Key::ArrowRight) => {
                if pos < buf.len() {
                    pos += 1;
                    redraw(&buf, pos);
                }
            }
            Ok(console::Key::Home) => {
                pos = 0;
                redraw(&buf, pos);
            }
            Ok(console::Key::End) => {
                pos = buf.len();
                redraw(&buf, pos);
            }
            Ok(console::Key::Escape) => {
                eprintln!();
                return initial.to_string();
            }
            _ => {}
        }
    }

    buf.into_iter().collect()
}

/// Display a formatted table of probe results. Returns the number of lines output.
/// Display a formatted table of probe results. Returns the number of lines output.
/// highlight col mapping: 0=inc, 1=icon, 2=name, 3=endpoint, 4=desc  (STATUS is not navigable)
fn display_probe_table(results: &[ProbeResult], highlight: Option<(usize, u8)>) -> usize {
    if results.is_empty() { return 0; }

    let term_w = std::env::var("COLUMNS")
        .ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(80);

    let w_status = 10usize;
    let w_inc = 3usize;
    let w_icon = 4usize;
    // 7 │ separators + fixed columns = 24 chars overhead; remaining split 3 ways
    let flex = term_w.saturating_sub(24).max(30);
    let w_name = results.iter().map(|r| display_w(&r.name)).max().unwrap_or(0)
        .clamp(10, flex * 25 / 100).max(10);
    let w_ep = results.iter().map(|r| display_w(&r.endpoint)).max().unwrap_or(0)
        .clamp(16, flex * 40 / 100).max(16);
    let w_desc = results.iter().map(|r| display_w(&r.description)).max().unwrap_or(0)
        .clamp(12, flex * 35 / 100).max(12);

    // Cell helpers: produce a string of exactly `w` visible characters.
    // ANSI escapes are preserved but don't count toward visual width.
    let cell_r = |s: &str, w: usize| -> String {
        let dw = display_w(s).min(w);
        format!("{}{}", " ".repeat(w - dw), s)
    };
    let cell_l = |s: &str, w: usize| -> String {
        let dw = display_w(s).min(w);
        format!("{}{}", s, " ".repeat(w - dw))
    };
    let cell_hl = |inner: &str, is_hl: bool| -> String {
        if is_hl { format!("\x1b[7m{}\x1b[0m", inner) } else { inner.to_string() }
    };

    let dash = |w: usize| "─".repeat(w);
    let top = format!("┌{}┬{}┬{}┬{}┬{}┬{}┐",
        dash(w_status), dash(w_inc), dash(w_icon), dash(w_name), dash(w_ep), dash(w_desc));
    let sep = format!("├{}┼{}┼{}┼{}┼{}┼{}┤",
        dash(w_status), dash(w_inc), dash(w_icon), dash(w_name), dash(w_ep), dash(w_desc));
    let bot = format!("└{}┴{}┴{}┴{}┴{}┴{}┘",
        dash(w_status), dash(w_inc), dash(w_icon), dash(w_name), dash(w_ep), dash(w_desc));

    eprintln!();
    eprintln!("{}", yellow(&top));

    eprintln!("│{}│{}│{}│{}│{}│{}│",
        cell_r(&cyan("STATUS"), w_status),
        cell_l("INC", w_inc),
        cell_l("ICON", w_icon),
        cell_l(&cyan("NAME"), w_name),
        cell_l("ENDPOINT", w_ep),
        cell_l("DESC", w_desc),
    );

    eprintln!("{}", yellow(&sep));

    for (i, r) in results.iter().enumerate() {
        let inc = if r.selected { " \u{2714} " } else { "   " };
        let status = if r.alive {
            cell_r(&bold_green("alive"), w_status)
        } else {
            cell_r(&bold_red("down"), w_status)
        };
        let hl_inc = highlight.is_some_and(|(row, col)| row == i && col == 0);
        let hl_icon = highlight.is_some_and(|(row, col)| row == i && col == 1);
        let hl_name = highlight.is_some_and(|(row, col)| row == i && col == 2);
        let hl_ep = highlight.is_some_and(|(row, col)| row == i && col == 3);
        let hl_desc = highlight.is_some_and(|(row, col)| row == i && col == 4);

        eprintln!("│{}│{}│{}│{}│{}│{}│",
            // STATUS column is not editable — never highlighted
            status,
            cell_hl(&cell_l(inc, w_inc), hl_inc),
            cell_hl(&cell_l(&r.icon, w_icon), hl_icon),
            cell_hl(&cell_l(&r.name, w_name), hl_name),
            cell_hl(&cell_l(&r.endpoint, w_ep), hl_ep),
            cell_hl(&cell_l(&r.description, w_desc), hl_desc),
        );
        if i < results.len() - 1 {
            eprintln!("{}", yellow(&sep));
        }
    }

    eprintln!("{}", yellow(&bot));

    let selected = results.iter().filter(|r| r.selected).count();
    let alive = results.iter().filter(|r| r.alive).count();
    eprintln!("  {}/{} selected, {}/{} reachable", selected, results.len(), alive, results.len());

    // line count: blank + top + header + sep + (data*2 - 1) + bot + summary
    3 + results.len() * 2
}

/// Interactive TUI editor for the probe results table.
/// Saves splash-state.json after each edit so edits persist across runs.
fn edit_probe_table(
    results: &mut [ProbeResult],
    services: &mut [SplashService],
    state: &mut SplashState,
    state_path: &Path,
) {
    use console::Term;

    let term = match Term::stdout().read_key() {
        Ok(_) => Term::stdout(),
        Err(_) => return, // not a TTY
    };

    let mut row: usize = 0;
    let mut col: u8 = 0; // 0=inc, 1=icon, 2=name, 3=endpoint, 4=desc (5 navigable columns)

    loop {
        // Save cursor, clear below, redraw
        eprint!("\x1b[s\x1b[J");
        display_probe_table(results, Some((row, col)));
        eprintln!("\n  \x1b[90m[↑↓/jk] nav  [←→/tab] field  [enter] edit  [s] toggle  [a] all  [q] done\x1b[0m");

        match term.read_key() {
            Ok(console::Key::ArrowUp) | Ok(console::Key::Char('k')) => {
                row = row.saturating_sub(1);
            }
            Ok(console::Key::ArrowDown) | Ok(console::Key::Char('j')) => {
                if row + 1 < results.len() { row += 1; }
            }
            Ok(console::Key::ArrowLeft) | Ok(console::Key::Char('h')) => {
                if col == 0 { col = 4; } else { col -= 1; }
            }
            Ok(console::Key::ArrowRight) | Ok(console::Key::Char('l'))
            | Ok(console::Key::Tab) => {
                col = (col + 1) % 5;
            }
            Ok(console::Key::Char('s')) => {
                results[row].selected = !results[row].selected;
                state.services = services.to_vec();
                if let Ok(json) = serde_json::to_string_pretty(&state) {
                    let _ = std::fs::write(state_path, json);
                }
            }
            Ok(console::Key::Char('a')) => {
                for r in results.iter_mut() { r.selected = true; }
                state.services = services.to_vec();
                if let Ok(json) = serde_json::to_string_pretty(&state) {
                    let _ = std::fs::write(state_path, json);
                }
            }
            Ok(console::Key::Enter) => {
                if col == 0 {
                    // INC column: toggle selection
                    results[row].selected = !results[row].selected;
                    state.services = services.to_vec();
                    if let Ok(json) = serde_json::to_string_pretty(&state) {
                        let _ = std::fs::write(state_path, json);
                    }
                } else if col == 1 {
                    // Icon: show palette, let user pick or type custom
                    eprint!("\x1b[J");
                    let palette = [
                        "🤖","🐳","🖥️","📊","📈","🔀","🐙","🗄️","📁","🎮",
                        "🎵","📹","🔒","🗃️","🌐","🛠️","📝","💾","🖨️","🖼️",
                        "📡","🔧","📋","🏠","⚙️","🔐","🔗","📨","🗂️","💻",
                    ];
                    eprintln!("  Icon palette:");
                    for (i, chunk) in palette.chunks(10).enumerate() {
                        eprintln!("  {}",
                            chunk.iter().enumerate()
                                .map(|(j, icon)| format!("[{}]{}", i * 10 + j, icon))
                                .collect::<Vec<_>>().join(" "));
                    }
                    eprint!("\n  Current: {}  Pick index or type custom icon: ", results[row].icon);
                    std::io::stdout().flush().ok();

                    let mut pick = String::new();
                    let _ = std::io::stdin().read_line(&mut pick);
                    let pick = pick.trim().to_string();

                    let new_icon = if let Ok(idx) = pick.parse::<usize>() {
                        palette.get(idx).map(|s| s.to_string()).unwrap_or(pick)
                    } else if pick.is_empty() {
                        results[row].icon.clone()
                    } else {
                        pick
                    };

                    if !new_icon.is_empty() {
                        results[row].icon = new_icon.clone();
                        services[row].icon = new_icon;
                        state.services = services.to_vec();
                        if let Ok(json) = serde_json::to_string_pretty(&state) {
                            let _ = std::fs::write(state_path, json);
                        }
                    }
                } else {
                    let (prompt, current) = match col {
                        2 => ("New name", results[row].name.clone()),
                        3 => ("New endpoint URL", results[row].endpoint.clone()),
                        4 => ("New description", results[row].description.clone()),
                        _ => continue,
                    };

                    // Clear the help bar, show prompt with current value as pre-filled input
                    eprint!("\x1b[J");

                    let new_val = read_line_with_initial(prompt, &current);

                    if !new_val.is_empty() {
                        match col {
                            2 => {
                                results[row].name = new_val.clone();
                                services[row].name = new_val;
                            }
                            3 => {
                                results[row].endpoint = new_val.clone();
                                results[row].alive = if new_val != "n/a" {
                                    eprint!(".");
                                    probe_http(&new_val)
                                } else {
                                    false
                                };
                                services[row].web_probe_url = Some(new_val);
                            }
                            4 => {
                                results[row].description = new_val.clone();
                                services[row].desc = new_val;
                            }
                            _ => {}
                        }
                        // Persist edits to splash-state.json
                        state.services = services.to_vec();
                        if let Ok(json) = serde_json::to_string_pretty(&state) {
                            let _ = std::fs::write(state_path, json);
                        }
                    }
                }
            }
            Ok(console::Key::Char('q')) | Ok(console::Key::Escape) => break,
            _ => {}
        }
    }

    // Final cleanup: clear the interactive table and draw the final static version
    eprint!("\x1b[s\x1b[J");
    display_probe_table(results, None);
}

fn probe_and_display_services(
    services: &mut [SplashService],
    hostname: &str,
    has_tty: bool,
    state: &mut SplashState,
    state_path: &Path,
) -> Vec<ProbeResult> {
    if services.is_empty() {
        return Vec::new();
    }

    eprintln!("\n{}", bold_cyan("Probing service endpoints..."));

    let mut results: Vec<ProbeResult> = Vec::with_capacity(services.len());

    for svc in services.iter() {
        let url = build_probe_url(svc, hostname);
        let endpoint = url.clone().unwrap_or_else(|| "n/a".to_string());

        let alive = if let Some(u) = url.as_ref() {
            eprint!(".");
            std::io::stdout().flush().ok();
            probe_http(u)
        } else {
            svc.port.is_some()
        };

        results.push(ProbeResult {
            name: svc.name.clone(),
            alive,
            icon: svc.icon.clone(),
            endpoint,
            description: svc.desc.clone(),
            selected: true,
        });
    }
    eprintln!();

    display_probe_table(&results, None);

    if has_tty {
        edit_probe_table(&mut results, services, state, state_path);

        // Filter deselected out of the results list (caller filters services by name)
        results.retain(|r| r.selected);
    }

    results
}

fn bold_cyan(s: &str) -> String { format!("\x1b[1;36m{}\x1b[0m", s) }
fn bold_red(s: &str) -> String { format!("\x1b[1;31m{}\x1b[0m", s) }

/// Word-wrap text to a given visual width (accounting for emoji/ANSI).
fn word_wrap(text: &str, max_width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let terminal_width = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(80);
    let wrap_w = max_width.min(terminal_width.saturating_sub(4));

    for paragraph in text.split("\n\n") {
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        for word in &words {
            let display_len = display_w(word);
            let line_len = display_w(&current_line);
            if current_line.is_empty() {
                current_line.push_str(word);
            } else if line_len + 1 + display_len <= wrap_w {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                lines.push(current_line.clone());
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }
        // Blank line between paragraphs
        if !paragraph.trim().is_empty() {
            lines.push(String::new());
        }
    }
    lines
}

/// Print the agent system prompt in a nice bordered box.
fn show_agent_prompt_box(prompt: &str) {
    let terminal_width = std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(80);
    let box_w = (terminal_width - 4).max(40);
    let pad_n = |n: usize| " ".repeat(n);

    // Top border
    eprintln!("\n\x1b[1;33m┌─\x1b[0m{}\x1b[1;33m┐\x1b[0m", "\u{2500}".repeat(box_w + 1));

    // Title row
    let title = "  Agent System Prompt ";
    let t_w = display_w(title);
    let left_pad = (box_w - t_w) / 2;
    eprintln!("\x1b[1;33m├─\x1b[0m{}\x1b[1;36m{}\x1b[0m{}\x1b[1;33m┤\x1b[0m", pad_n(left_pad), title, pad_n(box_w - t_w - left_pad + 1));

    // Prompt lines
    let wrapped = word_wrap(prompt, box_w.max(20));
    for line in &wrapped {
        let l_w = display_w(line);
        let r_pad = box_w - l_w;
        eprintln!("\x1b[1;33m│ \x1b[0m\x1b[90m{}\x1b[0m{} \x1b[1;33m│\x1b[0m", line, pad_n(r_pad));
    }

    // Bottom border
    eprintln!("\x1b[1;33m└─\x1b[0m{}\x1b[1;33m┘\x1b[0m", "\u{2500}".repeat(box_w + 1));
}

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
                let start = maybe_arr.find('[').unwrap_or(0);
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

/// Convert a selected GUI addon into a SplashService entry.
fn create_gui_service(addon: &GuiAddon, hostname: &str) -> SplashService {
    SplashService {
        name: addon.name.clone(),
        desc: format!("{}:{}", hostname, addon.src_port),
        icon: addon.icon.clone(),
        protocol: "http".to_string(),
        port: Some(addon.src_port.to_string()),
        host_override: None,
        base_path: Some("/".to_string()),
        group: "".to_string(),
        web_probe_url: None,
    }
}

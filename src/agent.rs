/// Probe common OpenAI-compatible endpoints on localhost/loopback.
pub(crate) fn probe_agent_endpoints() -> Vec<String> {
    let ports = [11434, 8000, 5555, 7777, 8080, 9000, 1234];
    let mut found = Vec::new();

    for port in ports {
        let addr = format!("http://127.0.0.1:{port}");
        match reqwest::blocking::get(format!("{addr}/api/tags")) {
            Ok(resp) if resp.status() == 200 => found.push(addr),
            _ => {}
        }
    }

    // Also check env var
    if let Ok(url) = std::env::var("OLLAMA_HOST") {
        let trimmed = url.trim().to_string();
        if !found.iter().any(|u| u == &trimmed) && is_openai_compatible(&trimmed) {
            found.push(trimmed);
        }
    }

    // Check http://localhost:port too
    for port in ports {
        let addr = format!("http://localhost:{port}");
        if !found.contains(&addr) && is_openai_compatible(&addr) {
            found.push(addr);
        }
    }

    found
}

/// Ask the agent API "list models" to verify it's an OpenAI-compatible endpoint.
pub(crate) fn is_openai_compatible(base_url: &str) -> bool {
    use reqwest::blocking::Client;
    let client = Client::new();

    // Check Ollama-style /api/tags first
    if let Ok(resp) = client.get(format!("{base_url}/api/tags")).send() {
        if resp.status() == 200 { return true; }
    }

    // Check OpenAI-style /v1/models
    if let Ok(resp) = client.get(format!("{base_url}/v1/models")).send() {
        if resp.status() == 200 { return true; }
    }

    false
}

/// Fetch list of models from an OpenAI-compatible endpoint.
pub(crate) fn get_models(base_url: &str) -> anyhow::Result<Vec<String>> {
    use reqwest::blocking::Client;
    let client = Client::new();

    let url = format!("{base_url}/v1/models");
    let resp = client.get(&url).send()?;
    
    if !resp.status().is_success() {
        return Err(anyhow::anyhow!("API returned {} for {}", resp.status(), url));
    }
    
    let body: serde_json::Value = resp.json()?;
    let mut models = Vec::new();

    // Ollama style
    if let Some(arr) = body.get("models").and_then(|v| v.as_array()) {
        for m in arr {
            if let Some(n) = m.get("name").and_then(|v| v.as_str()) {
                models.push(n.to_string());
            }
        }
    }

    // OpenAI style
    if let Some(arr) = body.get("data").and_then(|v| v.as_array()) {
        for m in arr {
            if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                models.push(id.to_string());
            }
        }
    }

    // Ollama without "models" wrapper — top level array
    if models.is_empty() {
        if let Some(arr) = body.as_array() {
            for m in arr {
                if let Some(n) = m.get("name").or_else(|| m.get("id")).and_then(|v| v.as_str()) {
                    models.push(n.to_string());
                }
            }
        }
    }

    Ok(models)
}

/// Send system command output to an AI agent for service analysis, returning the parsed JSON response.
#[allow(dead_code)]
pub(crate) fn analyze_services(
    base_url: &str,
    commands: &[String],
    prompt: &str,
) -> anyhow::Result<String> {
    use reqwest::blocking::Client;
    let client = Client::new();

    // Discover first available model so we can set one (empty string is rejected by some APIs)

    let system_prompt = r#"You are a system analyst. The user will provide you with raw output from Linux commands (systemctl list-units, systemctl --user list-units, docker ps, etc.). 

Your task: Return a compact JSON array of services that would be interesting to display on a server dashboard page. Only include real services — NOT the agent itself and not analysis metadata.

Each entry in the array must have exactly these fields:
- "name": human-readable service name (e.g., "Ollama")
- "desc": brief description (5–12 words) 
- "icon": a single emoji suited to the service
- "protocol": http, https, ssh, vnc, mqtt, websocket, etc.
- "port": server port number as string, or null if daemon-only
- "host_override": optional hostname override as string (null to infer from user's input)
- "base_path": optional URL path (null for root)
- "group": categorize into one of: ["Host Services", "Remote & Guest", "Monitoring", "Development", "Storage & Sharing"]
- "web_probe_url": URL to HTTP health-check probe (null if daemon-only, n/a, or SSH/VNC/etc.)

Rules:
1. ONLY include services with visible UIs, web endpoints, SSH access, VNC, MQTT, etc.
2. Daemon-only services without any UI go in the output but set port=null and a brief note about their purpose.
3. Do NOT invent ports, protocols, or hostnames.
4. Include GPU/VRAM info if the machine has GPUs and monitoring exists (like Glances).
5. Be concise — 8–20 services max unless there's genuinely more interesting ones.
6. If Docker containers are active, include relevant exposed services but mark them as "via Docker".

Return ONLY valid JSON — no markdown fences, no explanation text."#;

    let resp = client.post(format!("{base_url}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "",  // will be filled if known
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": format!(
                    "Analyze these service outputs and return a JSON array of interesting services.\n\nHost: {}\nMachine Info:\n{}\n\nServices:\n{}",
                    prompt,
                    commands.first().map(|s| s.as_str()).unwrap_or(""),
                    if commands.len() > 1 { commands[1..].join("\n") } else { String::new() }
                )},
            ],
            "temperature": 0.2,
        }))
        .send()?;
    
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text()?;
        return Err(anyhow::anyhow!("API error: {}\n{}", status, body));
    }
    let body: serde_json::Value = resp.json()?;
    Ok(body["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
}

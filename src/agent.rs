/// Normalize an endpoint URL so that 127.0.0.1 and localhost resolve to the same key.
fn normalize_key(url: &str) -> Option<String> {
    // Strip http:// or https:// prefix, extract host:port
    let stripped = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://"))?;
    Some(stripped.to_lowercase()) // host:port as canonical key
}

/// Probe common OpenAI-compatible endpoints on localhost/loopback.
pub(crate) fn probe_agent_endpoints() -> Vec<String> {
    let ports = [11434, 8000, 5555, 7777, 8080, 9000, 1234];
    let mut found: Vec<(String, String)> = Vec::new(); // (canonical_key, original_url)

    for port in ports {
        let addr = format!("http://127.0.0.1:{port}");
        if let Ok(resp) = reqwest::blocking::get(format!("{addr}/api/tags")) {
            if resp.status() == 200 {
                if let Some(key) = normalize_key(&addr) {
                    // Only add if no other URL with the same port already exists
                    if !found.iter().any(|(k, _)| k == &key) {
                        found.push((key, addr));
                    }
                }
            }
        }
    }

    // Also check env var
    if let Ok(url) = std::env::var("OLLAMA_HOST") {
        let trimmed = url.trim().to_string();
        if is_openai_compatible(&trimmed) && !found.iter().any(|(_, u)| *u == trimmed) {
            if let Some(key) = normalize_key(&trimmed) {
                if !found.iter().any(|(k, _)| k == &key) {
                    found.push((key, trimmed));
                }
            } else {
                found.push((trimmed.clone(), trimmed));
            }
        }
    }

    // Check http://localhost:port too, but dedup by port against 127.0.0.1:port
    for port in ports {
        let addr = format!("http://localhost:{port}");
        if is_openai_compatible(&addr) {
            let key = format!("localhost:{port}");
            // Only add if no 127.0.0.1:port variant already found
            let existing = found.iter().any(|(k, _)| k == &key);
            let conflict = format!("127.0.0.1:{port}");
            let no_conflict_for_localhost = !found.iter().any(|(_k, u)| {
                *u == format!("http://127.0.0.1:{port}") && normalize_key(u).as_deref() == Some(&conflict.to_lowercase())
            });
            if !existing && no_conflict_for_localhost {
                found.push((key, addr));
            }
        }
    }

    found.into_iter().map(|(_, url)| url).collect()
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

/// Get the currently loaded model from Ollama.
/// /api/ps returns models actively resident in VRAM.
pub(crate) fn get_default_model(base_url: &str) -> Option<String> {
    use reqwest::blocking::Client;
    let client = Client::new();

    // Ollama /api/ps - currently loaded models
    if let Ok(resp) = client.get(format!("{base_url}/api/ps")).send() {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                if let Some(arr) = body.get("models").and_then(|v| v.as_array()) {
                    for m in arr {
                        if let Some(name) = m.get("name").and_then(|v| v.as_str()) {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }

    // Fallback: first model from tags endpoint
    if let Ok(resp) = client.get(format!("{base_url}/api/tags")).send() {
        if resp.status().is_success() {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                if let Some(arr) = body.get("models").and_then(|v| v.as_array()) {
                    if let Some(first) = arr.first() {
                        if let Some(name) = first.get("name").and_then(|v| v.as_str()) {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }

    None
}




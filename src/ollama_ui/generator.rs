use std::fs;
use std::path::Path;

/// Escape HTML special characters in a string for safe insertion into HTML content.
pub(crate) fn esc(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['&', '<', '>', '"']) {
        std::borrow::Cow::Owned(
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;"),
        )
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Generate the Ollama dashboard HTML page and copy supporting CSS into the output directory.
pub(crate) fn generate(
    output_dir: &std::path::Path,
    hostname: &str,
    port: u16,
    glances_port: Option<u16>,
) -> Result<String, String> {
    use std::path::PathBuf;

    let mut html = include_str!("../ollama-ui/template.html").to_string();

    // Replace placeholders — try hostname_override first, fall back to hostname
    let hn_esc = esc(hostname).into_owned();
    for _ in 0..3 {
        html = html.replace("<!-- HOSTNAME -->", &hn_esc);
    }
    html = html.replace("<!-- PORT -->", &port.to_string());
    html = html.replace(
        "<!-- GLANCES_PORT -->",
        &glances_port.unwrap_or(61208).to_string(),
    );

    // Create output directory tree: <output_dir>/ollama-ui/
    let ollama_ui_dir = PathBuf::from(output_dir).join("ollama-ui");
    fs::create_dir_all(&ollama_ui_dir).map_err(|e| format!("Failed to create output dir: {e}"))?;

    // Write index.html into <output_dir>/ollama-ui/
    let out_path = ollama_ui_dir.join("index.html");
    fs::write(&out_path, &html).map_err(|e| format!("Failed to write file: {e}"))?;

    // Copy css/style.css into <output_dir>/ollama-ui/css/
    let css_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ollama-ui/css/style.css");
    if css_src.exists() {
        let css_dest_dir = ollama_ui_dir.join("css");
        fs::create_dir_all(&css_dest_dir).map_err(|e| format!("Failed to create css dir: {e}"))?;
        fs::copy(&css_src, css_dest_dir.join("style.css"))
            .map_err(|e| format!("Failed to copy css: {e}"))?;
    }

    Ok(out_path.to_string_lossy().to_string())
}

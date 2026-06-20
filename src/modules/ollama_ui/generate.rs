use std::fs;
use std::path::{Path, PathBuf};

/// Escape HTML special characters in a string for safe insertion into HTML content.
fn esc(s: &str) -> std::borrow::Cow<'_, str> {
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
    output_dir: &Path,
    hostname: &str,
    port: u16,
    service_port: u16,
    glances_port: Option<u16>,
) -> Result<String, String> {
    let mut html = include_str!("template.html").to_string();

    // Replace placeholders
    let hn_esc = esc(hostname).into_owned();
    for _ in 0..3 {
        html = html.replace("<!-- HOSTNAME -->", &hn_esc);
    }
    // The HTML template expects the API port (11434) or a specific structure for the label
    // To fix the user's issue, we ensure the PORT placeholder specifically populates the Ollama service port.
    html = html.replace("<!-- PORT -->", &service_port.to_string());
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

    // Copy style.css into <output_dir>/ollama-ui/css/
    let css_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/modules/ollama_ui/style.css");
    if css_src.exists() {
        let css_dest_dir = ollama_ui_dir.join("css");
        fs::create_dir_all(&css_dest_dir).map_err(|e| format!("Failed to create css dir: {e}"))?;
        fs::copy(&css_src, css_dest_dir.join("style.css"))
            .map_err(|e| format!("Failed to copy css: {e}"))?;
    }

    Ok(out_path.to_string_lossy().to_string())
}

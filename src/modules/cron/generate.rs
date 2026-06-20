use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn generate(
    output_dir: &Path,
    hostname: &str,
    port: u16,
    service_port: u16,
    _glances_port: Option<u16>,
) -> Result<String, String> {
    let mut html = include_str!("template.html").to_string();

    html = html.replace("<!-- HOSTNAME -->", hostname);
    html = html.replace("<!-- PORT -->", &port.to_string());

    let module_dir = PathBuf::from(output_dir).join("cron");
    fs::create_dir_all(&module_dir).map_err(|e| format!("Failed to create output dir: {e}"))?;

    // Write index.html
    let out_path = module_dir.join("index.html");
    fs::write(&out_path, &html).map_err(|e| format!("Failed to write file: {e}"))?;

    // Copy style.css
    let css_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/modules/cron/style.css");
    if css_src.exists() {
        let css_dest_dir = module_dir.join("css");
        fs::create_dir_all(&css_dest_dir).map_err(|e| format!("Failed to create css dir: {e}"))?;
        fs::copy(&css_src, css_dest_dir.join("style.css"))
            .map_err(|e| format!("Failed to copy css: {e}"))?;
    }

    // Process backend.py as a template (replace PORT placeholder)
    let backend_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/modules/cron/backend.py");
    if backend_src.exists() {
        let mut backend_content = fs::read_to_string(&backend_src)
            .map_err(|e| format!("Failed to read backend: {e}"))?;
        backend_content = backend_content.replace("<!-- PORT -->", &port.to_string());
        fs::write(module_dir.join("backend.py"), &backend_content)
            .map_err(|e| format!("Failed to write backend: {e}"))?;
    }

    Ok(out_path.to_string_lossy().to_string())
}

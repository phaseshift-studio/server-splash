mod ollama_ui;
mod systemd;
mod cron;

use std::path::Path;

/// A dashboard module that generates its own HTML page.
pub(crate) struct DashboardModule {
    pub name: String,
    pub description: String,
    pub default_port: u16,
    pub default_service_port: u16,
    pub icon: String,
    pub url_prefix: String,
    pub generate: fn(
        output_dir: &Path,
        hostname: &str,
        port: u16,
        service_port: u16,
        glances_port: Option<u16>,
    ) -> Result<String, String>,
}

/// Return all available dashboard modules.
pub(crate) fn all_modules() -> Vec<DashboardModule> {
    vec![
        ollama_ui::module_info(),
        systemd::module_info(),
        cron::module_info(),
    ]
}

mod ollama_ui;
mod systemd;
mod cron;

use std::path::Path;

/// What every sub-dashboard generator implements.
///
/// Named rather than written inline: five parameters in a field position trips
/// `clippy::type_complexity`, and CI lints with `-D warnings`.
pub(crate) type GenerateFn = fn(
    output_dir: &Path,
    hostname: &str,
    port: u16,
    service_port: u16,
    glances_port: Option<u16>,
) -> Result<String, String>;

/// A dashboard module that generates its own HTML page.
pub(crate) struct DashboardModule {
    pub name: String,
    pub description: String,
    pub default_port: u16,
    pub default_service_port: u16,
    pub icon: String,
    pub url_prefix: String,
    pub generate: GenerateFn,
}

/// Return all available dashboard modules.
pub(crate) fn all_modules() -> Vec<DashboardModule> {
    vec![
        ollama_ui::module_info(),
        systemd::module_info(),
        cron::module_info(),
    ]
}

mod generate;

use crate::modules::DashboardModule;

#[derive(serde::Deserialize)]
struct ModuleConfig {
    name: String,
    description: String,
    default_port: u16,
    icon: String,
    url_prefix: String,
}

pub(crate) fn module_info() -> DashboardModule {
    let cfg: ModuleConfig = serde_json::from_str(include_str!("module.json"))
        .expect("invalid ollama-ui/module.json");
    DashboardModule {
        name: cfg.name,
        description: cfg.description,
        default_port: cfg.default_port,
        icon: cfg.icon,
        url_prefix: cfg.url_prefix,
        generate: generate::generate,
    }
}

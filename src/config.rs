use serde::{Deserialize, Serialize};
use std::fs;

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:11434";

#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct SplashConfig {
    #[serde(default = "default_agent_url")]
    pub agent_url: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub output_dir: Option<String>,
    #[serde(default)]
    pub gui_pairs_file: Option<String>,
    #[serde(default)]
    pub glances_api_base: Option<String>,
}

fn default_agent_url() -> String {
    std::env::var("SPLASH_AGENT_URL")
        .unwrap_or(DEFAULT_AGENT_URL.to_string())
}

impl SplashConfig {
    pub(crate) fn load() -> Self {
        let cfg_dir = dirs::config_dir().map(|d| d.join("server-splash"));
        if let Some(ref path) = cfg_dir {
            let f = path.join("splash-config.toml");
            if f.exists() && !f.starts_with("/var/www/") {
                if let Ok(contents) = fs::read_to_string(&f) {
                    if let Ok(parsed) = toml::from_str::<SplashConfig>(&contents) {
                        return parsed;
                    }
                }
            }
        }

        SplashConfig {
            agent_url: default_agent_url(),
            hostname: None,
            output_dir: None,
            gui_pairs_file: None,
            glances_api_base: None,
        }
    }

    pub(crate) fn save_config(&self) {
        let home = dirs::config_dir().unwrap_or_default();
        let cfg_dir = home.join("server-splash");
        fs::create_dir_all(&cfg_dir).ok();
        let cfg_path = cfg_dir.join("splash-config.toml");
        if let Ok(s) = toml::to_string_pretty(self) {
            // Make sure we use the config gui_pairs_file path for persisting
            fs::write(&cfg_path, s).ok();
            println!("Config saved at {:?}", cfg_path);
        }
    }

    pub(crate) fn get_output_dir(&self, default: &str) -> String {
        match &self.output_dir {
            Some(d) => d.clone(),
            None => default.to_string(),
        }
    }
}

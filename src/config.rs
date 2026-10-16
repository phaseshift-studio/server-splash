use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:11434";
const DEFAULT_OUTPUT_FILE: &str = "index.html";

/// Default port for `--serve`.
///
/// Deliberately not 8080: that is one of the most commonly occupied ports on a Linux
/// box (nginx, Tomcat, countless app images), and shipping a default that collides on
/// first run is a bad first impression. `run_serve` also refuses to start on a port
/// something else owns, and names the occupant.
pub(crate) const DEFAULT_SERVE_PORT: u16 = 6420;

/// Persisted configuration, read from `~/.config/server-splash/splash-config.toml`.
///
/// Every field is optional so an existing config file from an older version keeps
/// working — unknown keys are ignored and missing ones fall back to the defaults below.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub(crate) struct SplashConfig {
    #[serde(default = "default_agent_url")]
    pub agent_url: String,
    /// Model to use for the optional enrichment pass. Empty means "ask the agent".
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub output_dir: Option<String>,
    /// Filename written into `output_dir`. Defaults to `index.html` so the page
    /// replaces the one served at `http://<host>/`.
    #[serde(default)]
    pub output_file: Option<String>,
    /// Directory the generated page is copied to (e.g. `/var/www/html`).
    #[serde(default)]
    pub deploy_path: Option<String>,
    /// Legacy: a list of GUI add-ons to append manually. Discovery supersedes this.
    #[serde(default)]
    pub gui_pairs_file: Option<String>,
    /// Hand-written curation rules. See `splash --scaffold`.
    #[serde(default)]
    pub overrides_file: Option<String>,
    #[serde(default)]
    pub glances_api_base: Option<String>,
    /// Appended to a bare hostname so links resolve from a browser: `ginger` ->
    /// `ginger.local`. Set to "" to disable. IPs and names already containing a dot
    /// are never touched.
    #[serde(default)]
    pub host_suffix: Option<String>,
    /// Page `<h1>` / `<title>`. Defaults to the hostname.
    #[serde(default)]
    pub title: Option<String>,
    /// Subtitle line. Defaults to a subtitle built from the machine profile.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Include services bound to loopback only (off by default).
    #[serde(default)]
    pub include_loopback: Option<bool>,
    /// Ports to leave off the page entirely, e.g. the web server hosting it.
    #[serde(default)]
    pub exclude_ports: Option<Vec<u16>>,
    /// Also list ports in the IANA dynamic range (32768–60999).
    #[serde(default)]
    pub include_ephemeral: Option<bool>,
    /// Probe each discovered port over HTTP to classify it (on by default).
    #[serde(default)]
    pub probe_http: Option<bool>,
    /// Per-port HTTP probe timeout.
    #[serde(default)]
    pub http_timeout_ms: Option<u64>,
    /// Where to keep copies of files that a deploy overwrites. Defaults to
    /// `~/.local/share/server-splash/backups` — never inside the web root, where a
    /// backup would be publicly readable.
    #[serde(default)]
    pub backup_dir: Option<String>,
    /// Port for `--serve`: regenerate continuously and serve the page from the tool
    /// itself. 0 (the default) means "generate once and exit", as before.
    #[serde(default)]
    pub serve_port: Option<u16>,
    /// Seconds between regenerations while serving. 0 disables the refresh loop.
    #[serde(default)]
    pub interval_secs: Option<u64>,
    /// Embed icons as data URIs instead of writing an `icons/` directory. On by
    /// default: a relative icon path only resolves when the page is served from the
    /// exact directory it was written to.
    #[serde(default)]
    pub inline_icons: Option<bool>,
    /// Size of the payload the in-page speed test downloads. 0 disables the button.
    #[serde(default)]
    pub speedtest_bytes: Option<u64>,
    /// Disable the pre-deploy backup entirely.
    #[serde(default)]
    pub no_backup: Option<bool>,
    /// Skip the LLM enrichment pass entirely, even in interactive mode.
    #[serde(default)]
    pub no_agent: Option<bool>,
    /// Sub-dashboards to build alongside the page, by module name
    /// (e.g. `["Ollama Dashboard"]`). The wizard selects these interactively.
    #[serde(default)]
    pub modules: Option<Vec<String>>,
    /// Where this config was read from, so saving writes back to the same file
    /// instead of always the default location (matters for `--config`).
    #[serde(skip)]
    pub source_path: Option<PathBuf>,
}

fn default_agent_url() -> String {
    std::env::var("SPLASH_AGENT_URL").unwrap_or(DEFAULT_AGENT_URL.to_string())
}

impl Default for SplashConfig {
    fn default() -> Self {
        SplashConfig {
            agent_url: default_agent_url(),
            model: None,
            hostname: None,
            output_dir: None,
            output_file: None,
            deploy_path: None,
            gui_pairs_file: None,
            overrides_file: None,
            glances_api_base: None,
            host_suffix: None,
            title: None,
            subtitle: None,
            include_loopback: None,
            exclude_ports: None,
            include_ephemeral: None,
            probe_http: None,
            http_timeout_ms: None,
            serve_port: None,
            interval_secs: None,
            inline_icons: None,
            speedtest_bytes: None,
            backup_dir: None,
            no_backup: None,
            no_agent: None,
            modules: None,
            source_path: None,
        }
    }
}

/// `~/.config/server-splash/` (or the platform equivalent).
pub(crate) fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("server-splash")
}

impl SplashConfig {
    /// Path of the config file itself.
    pub(crate) fn config_path() -> PathBuf {
        config_dir().join("splash-config.toml")
    }

    pub(crate) fn load() -> Self {
        let path = Self::config_path();
        if let Ok(contents) = fs::read_to_string(&path) {
            match toml::from_str::<SplashConfig>(&contents) {
                Ok(mut parsed) => {
                    parsed.source_path = Some(path);
                    return parsed;
                }
                Err(e) => eprintln!(
                    "  \x1b[31mwarning\x1b[0m: {} is not valid TOML ({e}); using defaults",
                    path.display()
                ),
            }
        }
        SplashConfig::default()
    }

    pub(crate) fn save_config(&self) {
        let dir = self
            .source_path
            .as_ref()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(config_dir);
        fs::create_dir_all(&dir).ok();
        if let Ok(s) = toml::to_string_pretty(self) {
            let path = self.source_path.clone().unwrap_or_else(|| dir.join("splash-config.toml"));
            match fs::write(&path, s) {
                Ok(()) => println!("Config saved at {}", path.display()),
                Err(e) => eprintln!("Could not save config to {}: {e}", path.display()),
            }
        }
    }

    pub(crate) fn get_output_dir(&self, default: &str) -> String {
        self.output_dir.clone().unwrap_or_else(|| default.to_string())
    }

    /// Suffix appended to a bare hostname. Defaults to `.local` (mDNS).
    pub(crate) fn host_suffix(&self) -> String {
        self.host_suffix.clone().unwrap_or_else(|| ".local".to_string())
    }

    pub(crate) fn get_output_file(&self) -> String {
        self.output_file
            .clone()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_OUTPUT_FILE.to_string())
    }

    pub(crate) fn get_title(&self, hostname: &str) -> String {
        self.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| hostname.to_string())
    }

    pub(crate) fn get_subtitle(&self) -> String {
        self.subtitle.clone().unwrap_or_default()
    }

    /// Where curation rules live. Defaults next to the config file.
    pub(crate) fn get_overrides_path(&self) -> PathBuf {
        self.overrides_file
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| config_dir().join("overrides.toml"))
    }

    pub(crate) fn include_loopback(&self) -> bool {
        self.include_loopback.unwrap_or(false)
    }

    pub(crate) fn excluded_ports(&self) -> Vec<u16> {
        self.exclude_ports.clone().unwrap_or_default()
    }

    pub(crate) fn include_ephemeral(&self) -> bool {
        self.include_ephemeral.unwrap_or(false)
    }

    pub(crate) fn probe_http(&self) -> bool {
        self.probe_http.unwrap_or(true)
    }

    pub(crate) fn http_timeout_ms(&self) -> u64 {
        self.http_timeout_ms.unwrap_or(1500)
    }

    /// Port to serve on, or `None` to generate once and exit.
    pub(crate) fn serve_port(&self) -> Option<u16> {
        self.serve_port.filter(|p| *p > 0)
    }

    /// Seconds between regenerations while serving. Defaults to 15 minutes; 0 keeps
    /// the first page and never regenerates.
    pub(crate) fn interval_secs(&self) -> u64 {
        self.interval_secs.unwrap_or(900)
    }

    pub(crate) fn inline_icons(&self) -> bool {
        self.inline_icons.unwrap_or(true)
    }

    /// Payload size for the in-page speed test. 8 MB measures a gigabit LAN in about
    /// 70 ms, which is long enough to be meaningful and short enough to be polite.
    pub(crate) fn speedtest_bytes(&self) -> u64 {
        self.speedtest_bytes.unwrap_or(8 * 1024 * 1024)
    }

    /// Directory that holds pre-deploy backups, or `None` when disabled.
    pub(crate) fn backup_dir(&self) -> Option<PathBuf> {
        if self.no_backup.unwrap_or(false) {
            return None;
        }
        Some(
            self.backup_dir
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    dirs::data_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("server-splash")
                        .join("backups")
                }),
        )
    }

    pub(crate) fn selected_modules(&self) -> Vec<String> {
        self.modules.clone().unwrap_or_default()
    }

    pub(crate) fn agent_enabled(&self) -> bool {
        !self.no_agent.unwrap_or(false)
    }
}

/// Looks up an environment variable. Injectable so the parsing below can be tested
/// without mutating the process environment, which would make tests race each other.
pub(crate) type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Read a variable, treating an empty value as unset.
fn env_str(get: Lookup, name: &str) -> Option<String> {
    get(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Accept the spellings people actually write in a compose file.
fn env_bool(get: Lookup, name: &str) -> Option<bool> {
    match env_str(get, name)?.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        other => {
            eprintln!("  \x1b[33mwarning\x1b[0m: {name}={other:?} is not a boolean; ignoring");
            None
        }
    }
}

fn env_num<T: std::str::FromStr>(get: Lookup, name: &str) -> Option<T> {
    let raw = env_str(get, name)?;
    match raw.parse::<T>() {
        Ok(v) => Some(v),
        Err(_) => {
            eprintln!("  \x1b[33mwarning\x1b[0m: {name}={raw:?} is not a number; ignoring");
            None
        }
    }
}

/// `80,443 8080` -> `[80, 443, 8080]`
fn env_port_list(get: Lookup, name: &str) -> Option<Vec<u16>> {
    let raw = env_str(get, name)?;
    let ports: Vec<u16> = raw
        .split([',', ' ', ';'])
        .filter(|p| !p.trim().is_empty())
        .filter_map(|p| match p.trim().parse::<u16>() {
            Ok(port) => Some(port),
            Err(_) => {
                eprintln!("  \x1b[33mwarning\x1b[0m: {name} contains {p:?}, which is not a port; skipping");
                None
            }
        })
        .collect();
    if ports.is_empty() { None } else { Some(ports) }
}

impl SplashConfig {
    /// Overlay `SPLASH_*` environment variables from the real environment.
    pub(crate) fn apply_env(&mut self) {
        self.apply_env_from(&|key| std::env::var(key).ok());
    }

    /// Overlay `SPLASH_*` variables from an injected source.
    ///
    /// A container is configured with environment variables rather than by editing a
    /// file inside the image, so these take precedence over the TOML config and are in
    /// turn overridden by explicit command-line flags.
    pub(crate) fn apply_env_from(&mut self, get: Lookup) {
        macro_rules! set {
            ($field:ident, $env:literal) => {
                if let Some(v) = env_str(get, $env) { self.$field = Some(v); }
            };
        }
        set!(hostname, "SPLASH_HOSTNAME");
        set!(host_suffix, "SPLASH_HOST_SUFFIX");
        set!(title, "SPLASH_TITLE");
        set!(subtitle, "SPLASH_SUBTITLE");
        set!(output_dir, "SPLASH_OUTPUT_DIR");
        set!(output_file, "SPLASH_OUTPUT_FILE");
        set!(deploy_path, "SPLASH_DEPLOY");
        set!(model, "SPLASH_MODEL");
        set!(glances_api_base, "SPLASH_GLANCES");
        set!(overrides_file, "SPLASH_OVERRIDES");
        set!(backup_dir, "SPLASH_BACKUP_DIR");

        // `agent_url` is a plain String rather than an Option, so it is not a `set!`.
        if let Some(v) = env_str(get, "SPLASH_AGENT_URL") { self.agent_url = v; }

        if let Some(v) = env_bool(get, "SPLASH_NO_AGENT") { self.no_agent = Some(v); }
        if let Some(v) = env_bool(get, "SPLASH_INCLUDE_LOOPBACK") { self.include_loopback = Some(v); }
        if let Some(v) = env_bool(get, "SPLASH_INCLUDE_EPHEMERAL") { self.include_ephemeral = Some(v); }
        if let Some(v) = env_bool(get, "SPLASH_PROBE_HTTP") { self.probe_http = Some(v); }
        if let Some(v) = env_bool(get, "SPLASH_INLINE_ICONS") { self.inline_icons = Some(v); }
        if let Some(v) = env_bool(get, "SPLASH_NO_BACKUP") { self.no_backup = Some(v); }

        if let Some(v) = env_port_list(get, "SPLASH_EXCLUDE_PORTS") { self.exclude_ports = Some(v); }
        if let Some(v) = env_num::<u64>(get, "SPLASH_HTTP_TIMEOUT_MS") { self.http_timeout_ms = Some(v); }
        if let Some(v) = env_num::<u64>(get, "SPLASH_SPEEDTEST_BYTES") { self.speedtest_bytes = Some(v); }
        if let Some(v) = env_num::<u16>(get, "SPLASH_PORT") { self.serve_port = Some(v); }
        if let Some(v) = env_num::<u64>(get, "SPLASH_INTERVAL") { self.interval_secs = Some(v); }
        if let Some(v) = env_str(get, "SPLASH_MODULES") {
            let modules: Vec<String> = v
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            if !modules.is_empty() { self.modules = Some(modules); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = SplashConfig::default();
        assert_eq!(c.get_output_file(), "index.html");
        assert_eq!(c.get_output_dir("./server-splash"), "./server-splash");
        assert!(!c.include_loopback(), "loopback services are opt-in");
        assert!(c.probe_http());
        assert!(c.agent_enabled());
        assert_eq!(c.get_title("ginger.local"), "ginger.local");
        assert_eq!(c.host_suffix(), ".local");
        assert_eq!(c.speedtest_bytes(), 8 * 1024 * 1024);
        assert!(c.inline_icons(), "a self-contained page is the safer default");
        assert_eq!(SplashConfig { speedtest_bytes: Some(0), ..SplashConfig::default() }.speedtest_bytes(), 0);
        assert!(c.backup_dir().is_some(), "backups are on by default");
        assert!(!c.backup_dir().unwrap().starts_with("/var/www"), "backups must not land in a web root");
    }

    /// Build a lookup over a fixed set of pairs.
    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn environment_configures_a_container() {
        let vars = [
            ("SPLASH_HOSTNAME", "ginger.local"),
            ("SPLASH_PORT", "8080"),
            ("SPLASH_INTERVAL", "600"),
            ("SPLASH_DEPLOY", "/var/www/html"),
            ("SPLASH_GLANCES", "http://ginger.local:61208"),
            ("SPLASH_EXCLUDE_PORTS", "80, 443 8080"),
            ("SPLASH_INCLUDE_LOOPBACK", "yes"),
            ("SPLASH_NO_AGENT", "1"),
            ("SPLASH_INLINE_ICONS", "on"),
            ("SPLASH_SPEEDTEST_BYTES", "0"),
            ("SPLASH_TITLE", "Home Server"),
            ("SPLASH_MODULES", "Ollama Dashboard, Another"),
        ];
        let mut cfg = SplashConfig::default();
        cfg.apply_env_from(&env_from(&vars));

        assert_eq!(cfg.hostname.as_deref(), Some("ginger.local"));
        assert_eq!(cfg.serve_port(), Some(8080));
        assert_eq!(cfg.interval_secs(), 600);
        assert_eq!(cfg.deploy_path.as_deref(), Some("/var/www/html"));
        assert_eq!(cfg.glances_api_base.as_deref(), Some("http://ginger.local:61208"));
        assert_eq!(cfg.excluded_ports(), vec![80, 443, 8080]);
        assert!(cfg.include_loopback());
        assert!(!cfg.agent_enabled());
        assert!(cfg.inline_icons());
        assert_eq!(cfg.speedtest_bytes(), 0);
        assert_eq!(cfg.get_title("ignored"), "Home Server");
        assert_eq!(cfg.selected_modules(), vec!["Ollama Dashboard", "Another"]);
    }

    #[test]
    fn environment_overrides_the_config_file() {
        let file = SplashConfig { hostname: Some("from-file".into()), ..SplashConfig::default() };
        let vars = [("SPLASH_HOSTNAME", "from-env")];
        let mut cfg = file;
        cfg.apply_env_from(&env_from(&vars));
        assert_eq!(cfg.hostname.as_deref(), Some("from-env"), "env beats the file");
    }

    #[test]
    fn malformed_environment_values_are_ignored_not_fatal() {
        let vars = [
            ("SPLASH_PORT", "not-a-port"),
            ("SPLASH_INTERVAL", "soon"),
            ("SPLASH_INCLUDE_LOOPBACK", "maybe"),
            ("SPLASH_EXCLUDE_PORTS", "80,banana"),
            ("SPLASH_HOSTNAME", "   "),
        ];
        let mut cfg = SplashConfig::default();
        cfg.apply_env_from(&env_from(&vars));

        assert_eq!(cfg.serve_port(), None, "a bad port must not be guessed at");
        assert_eq!(cfg.interval_secs(), 900, "falls back to the default");
        assert!(!cfg.include_loopback());
        assert_eq!(cfg.excluded_ports(), vec![80], "only the valid port survives");
        assert_eq!(cfg.hostname, None, "whitespace counts as unset");
    }

    #[test]
    fn old_config_files_still_load() {
        // The original shipped exactly these five keys; adding fields must not break it.
        let old = r#"
agent_url = "http://127.0.0.1:11434"
hostname = "ginger"
output_dir = "./server-splash"
gui_pairs_file = "/tmp/services.toml"
glances_api_base = "http://localhost:61208"
"#;
        let parsed: SplashConfig = toml::from_str(old).expect("legacy config must still parse");
        assert_eq!(parsed.hostname.as_deref(), Some("ginger"));
        assert_eq!(parsed.get_output_file(), "index.html", "new keys take defaults");
        assert_eq!(parsed.get_title("ginger"), "ginger");
    }

    #[test]
    fn explicit_settings_win() {
        let c = SplashConfig {
            output_file: Some("status.html".into()),
            title: Some("Home Server".into()),
            include_loopback: Some(true),
            exclude_ports: Some(vec![80, 443]),
            no_agent: Some(true),
            ..SplashConfig::default()
        };
        assert_eq!(c.get_output_file(), "status.html");
        assert_eq!(c.get_title("ignored"), "Home Server");
        assert!(c.include_loopback());
        assert_eq!(c.excluded_ports(), vec![80, 443]);
        assert!(!c.agent_enabled());

        let off = SplashConfig { no_backup: Some(true), ..SplashConfig::default() };
        assert!(off.backup_dir().is_none());

        let custom = SplashConfig { backup_dir: Some("/srv/backups".into()), ..SplashConfig::default() };
        assert_eq!(custom.backup_dir().unwrap(), PathBuf::from("/srv/backups"));
    }

    #[test]
    fn blank_strings_fall_back_to_defaults() {
        let c = SplashConfig {
            output_file: Some("   ".into()),
            title: Some(String::new()),
            ..SplashConfig::default()
        };
        assert_eq!(c.get_output_file(), "index.html");
        assert_eq!(c.get_title("ginger.local"), "ginger.local");
    }
}

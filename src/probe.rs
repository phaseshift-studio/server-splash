use std::process::Command;

/// A command probe to run on the host machine.
#[derive(Debug, Clone)]
pub(crate) struct CommandSpec {
    pub command: String,
    pub title: String,
}

impl CommandSpec {
    pub(crate) fn new(command: &str, title: &str) -> Self {
        Self {
            command: command.to_string(),
            title: title.to_string(),
        }
    }
}

/// Run a single command, return stdout (trimmed to 200 lines).
fn run_cmd(cmd: &str) -> Option<String> {
    let mut parts = cmd.split_whitespace();
    let program = parts.next()?;
    let args: Vec<&str> = parts.collect();
    let output = Command::new(program).args(&args).output().ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(200)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

impl CommandSpec {
    /// Run this probe command. Returns formatted output or error message.
    pub(crate) fn execute(&self) -> String {
        match run_cmd(&self.command) {
            Some(output) => format!("==== {} ====\n{}", self.title, output),
            None => format!("==== {} ====[command failed or not found]", self.title),
        }
    }
}

/// Gather all available probes from the system.
pub(crate) fn gather_probes() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "systemctl --no-pager --no-legend list-units --type=service",
            "Host services",
        ),
        CommandSpec::new(
            "systemctl --user --no-pager --no-legend list-units --type=service",
            "User services",
        ),
        CommandSpec::new(
            "docker ps --format '{{.Names}}\\t{{.Image}}\\t{{.Ports}}'",
            "Docker containers",
        ),
        CommandSpec::new(
            "free -m | head -5 && cat /proc/cpuinfo | grep 'model name' | head -1 && lsblk -d -o NAME,SIZE,TYPE | grep 'disk'",
            "System specs",
        ),
        CommandSpec::new("nvidia-smi --query-gpu=name,memory.total,memory.used,temperature.gpu,fan.speed,utilization.gpu --format=csv,noheader 2>/dev/null || echo 'No NVIDIA GPU detected'", "GPU/Sensors"),
        CommandSpec::new("uptime && cat /etc/os-release | grep PRETTY_NAME", "Uptime & OS"),
    ]
}

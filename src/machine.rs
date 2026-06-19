use sysinfo::System;

/// Static machine profile captured at generation time.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct MachineInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub total_memory_mb: u64,
    pub cpu_core_count: usize,
}

/// Collect machine profile from the host.
pub(crate) fn collect() -> MachineInfo {
    let sys = System::new_all();

    // OS name / kernel version
    let os_name = read_line("/etc/os-release")
        .and_then(|lines| {
            if lines.len() >= 2 {
                Some(format!("{} {}", lines[0].trim(), lines[1].trim()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| "Linux".to_string());

    let kernel_version = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| '?'.to_string());

    // Total memory in MiB
    let total_memory_mb = sys.total_memory() / (1024 * 1024);

    // CPU cores (logical)
    let cpu_core_count = sys.cpus().len();

    MachineInfo {
        hostname: std::env::var("HOSTNAME")
            .ok()
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "unknown".to_string()),
        os: os_name,
        kernel: kernel_version,
        total_memory_mb,
        cpu_core_count,
    }
}

fn read_line(path: &str) -> Option<Vec<String>> {
    std::fs::read_to_string(path)
        .ok()
        .map(|content| content.lines().take(3).map(String::from).collect())
}

/// Serialize MachineInfo to JSON for embedding in HTML.
pub(crate) fn to_json(info: &MachineInfo) -> String {
    serde_json::to_string(info).unwrap_or_else(|_| "{}".to_string())
}

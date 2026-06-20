use sysinfo::System;

/// Static machine profile captured at generation time.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct MachineInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub total_memory_mb: u64,
    pub cpu_core_count: usize,
    pub disk_total_gb: u64,
    pub disk_used_gb: u64,
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

    // Disk space — sum across all non-removable mount points
    let mut disk_total_gb: u64 = 0;
    let mut disk_used_gb: u64 = 0;
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    for disk in &disks {
        if disk.is_removable() { continue; }
        disk_total_gb += disk.total_space() / (1024 * 1024 * 1024);
        let used = disk.total_space().saturating_sub(disk.available_space());
        disk_used_gb += used / (1024 * 1024 * 1024);
    }

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
        disk_total_gb,
        disk_used_gb,
    }
}

fn read_line(path: &str) -> Option<Vec<String>> {
    std::fs::read_to_string(path)
        .ok()
        .map(|content| content.lines().take(3).map(String::from).collect())
}
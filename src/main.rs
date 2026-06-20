mod config;
mod probe;
mod wizard;
mod generator;
mod agent;
mod modules;
mod machine;

fn main() {
    // Print the splash header with color
    let raw = include_str!("header.txt");
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() { println!(); continue; }

        if i == 5 && line.contains("PhaseShift") {
            // Split the last line into art prefix + "PhaseShift Studio" at position 0
            let idx = line.find("PhaseShift").unwrap_or(0);
            let prefix = &line[..idx];
            let studio = "\x1b[38;5;214m\x1b[1mPhaseShift Studio\x1b[0m";

            let art: String = prefix.chars().map(|c| match c {
                ' ' => " ".to_string(),
                _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
            }).collect();
            println!("{art}{studio}");
        } else {
            // All other lines — bright green art on dark bg
            let colored: String = line.chars().map(|c| match c {
                ' ' => " ".to_string(),
                _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
            }).collect();
            println!("{colored}");
        }
    }

    let cfg = config::SplashConfig::load();

    // Run the wizard to gather host info and pick services
    let output = match wizard::run(&cfg) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("\nWizard error: {}", e);
            std::process::exit(1);
        }
    };

    // Generate the HTML splash page (writes splash-server.html to disk)
    let spl_path = match generator::generate(&output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\nError generating splash page: {e}");
            std::process::exit(1);
        }
    };

    // Generate selected dashboard modules
    let all_modules = modules::all_modules();
    for module_name in &output.selected_modules {
        if let Some(module) = all_modules.iter().find(|m| &m.name == module_name) {
            match (module.generate)(&output.output_dir, &output.hostname, module.default_port, module.default_service_port, None) {
                Ok(path) => println!("{} generated at: {path}", module.name),
                Err(e) => eprintln!("\nError generating {}: {e}", module.name),
            }

            // Inject splash page card for this module
            let out_path = output.output_dir.join("splash-server.html");
            if let Ok(splash_content) = std::fs::read_to_string(&out_path) {
                let card = generator::module_card(
                    &module.url_prefix,
                    &module.icon,
                    &module.name,
                    &module.description,
                    module.default_port,
                    module.default_service_port,
                );
                let with_card = splash_content.replace(
                    "<!--GROUP_HOST_SERVICES-->",
                    &format!("{}<!--GROUP_HOST_SERVICES-->", card),
                );
                let _ = std::fs::write(&out_path, &with_card);
            }
        }
    }

    // Deploy to target path if different from output dir
    if let Some(ref deploy_path) = output.deploy_path {
        let deploy = std::path::Path::new(deploy_path);
        if deploy != output.output_dir.as_path() {
            eprintln!("\n\x1b[1;36mDeploying to:\x1b[0m {}", deploy.display());
            // Recursively copy output dir contents to deploy path
            if let Err(e) = copy_dir_contents(&output.output_dir, deploy) {
                eprintln!("\x1b[1;31mDeploy error:\x1b[0m {e}");
            } else {
                eprintln!("\x1b[1;32mDeployed successfully.\x1b[0m");
                println!("\nSplash page deployed at: {}/splash-server.html", deploy.display());
                return;
            }
        }
    }

    println!("\nSplash page generated at: {}", spl_path);
}

fn copy_dir_contents(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("Failed to create deploy dir: {e}"))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("Failed to read source dir: {e}"))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {e}"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("Failed to copy {}: {e}", src_path.display()))?;
        }
    }
    Ok(())
}

mod config;
mod probe;
mod wizard;
mod generator;
mod agent;

fn main() {
    // Print the splash header with color
    let raw = include_str!("header.txt");
    for (i, line) in raw.lines().enumerate() {
        if line.trim().is_empty() { print!("\n"); continue; }

        if i == 5 && line.contains("PhaseShift") {
            // Split the last line into art prefix + "PhaseShift Studio" at position 0
            let idx = line.find("PhaseShift").unwrap_or(0);
            let prefix = &line[..idx];
            let studio = "\x1b[38;5;214m\x1b[1mPhaseShift Studio\x1b[0m";

            let art: String = prefix.chars().map(|c| match c {
                ' ' => " ".to_string(),
                _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
            }).collect();
            print!("{art}{studio}\n");
        } else {
            // All other lines — bright green art on dark bg
            let colored: String = line.chars().map(|c| match c {
                ' ' => " ".to_string(),
                _ => format!("\x1b[92m\x1b[1m{}\x1b[0m", c),
            }).collect();
            print!("{colored}\n");
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

    // Generate the HTML splash page
    let spl_path = match generator::generate(&output) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("\nError generating splash page: {e}");
            std::process::exit(1);
        }
    };

    // Generate the Ollama Dashboard if ollama is available on this host
    if output.ollama_dashboard_selected {
        match generator::generate_ollama_ui(&output.output_dir, &output.hostname, 11434, None) {
            Ok(path) => println!("Ollama dashboard generated at: {path}"),
            Err(e) => eprintln!("\nError generating ollama dashboard: {e}"),
        }
    }

    println!("\nSplash page generated at: {}", spl_path);
}

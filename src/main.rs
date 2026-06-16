mod config;
mod probe;
mod wizard;
mod generator;
mod agent;

fn main() {
    let cfg = config::SplashConfig::load();

    // Run the wizard to gather host info and pick services
    let output = match wizard::run(&cfg) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("\nWizard error: {}", e);
            std::process::exit(1);
        }
    };

    // Generate the HTML
    match generator::generate(&output) {
        Ok(path) => println!("\nSplash page generated at: {path}"),
        Err(e) => eprintln!("\nError generating splash page: {e}"),
    }
}

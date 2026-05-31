fn main() {
    if let Err(error) = simx::cli::run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

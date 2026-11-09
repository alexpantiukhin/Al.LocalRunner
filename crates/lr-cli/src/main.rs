use clap::Parser;

#[derive(Parser)]
#[command(name = "localrunner", version)]
struct Cli {}

fn main() {
    Cli::parse();
}

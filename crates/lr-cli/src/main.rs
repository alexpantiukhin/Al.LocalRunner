use clap::{CommandFactory, FromArgMatches};
use lr_cli::commands::{run, version_text, Cli};

#[tokio::main]
async fn main() {
    let version: &'static str = Box::leak(version_text().into_boxed_str());
    let matches = Cli::command().version(version).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    std::process::exit(run(cli).await);
}

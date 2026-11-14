use clap::Parser;

use offload::cli::Cli;
use offload::{config, detect, headless, tui};

fn main() {
    let args = Cli::parse();
    let result = if args.src.is_some() || args.dest.is_some() {
        // Headless mode: both paths required.
        match (args.src.clone(), args.dest.clone()) {
            (Some(src), Some(dest)) => headless::run_headless(&headless::HeadlessArgs {
                src,
                dest,
                dry_run: args.dry_run,
                rename: !args.no_rename,
                force: args.force,
                eject: args.eject,
            }),
            _ => {
                eprintln!("error: --src and --dest must be used together (omit both for the interactive UI)");
                std::process::exit(2);
            }
        }
    } else {
        let last_dest = config::load_config()
            .ok()
            .and_then(|c| c.last_dest);
        let source = match detect::detect_source() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: source detection failed: {e:#}");
                None
            }
        };
        tui::run_tui(source, last_dest, args.no_rename, args.force, args.dry_run)
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

//! pdast2mozzi — Convert a pdast JSON AST to a self-contained Mozzi
//! (Arduino sound-synthesis library) sketch (.ino), written in C++.
//!
//! Usage:
//!   pdast2mozzi [OPTIONS] <AST.json>
//!
//! The tool reads a JSON AST produced by pd2ast and generates one .ino
//! source file. Signal-rate (tilde) objects become Mozzi unit generators
//! computed once per `updateAudio()` call; control-rate objects become a
//! real message-passing graph (see mozzi_gen.rs docs) driven by
//! `updateControl()` plus a small `pd_*` hook-function ABI for MIDI/param
//! integration — see README.md for how to wire those up from your own
//! sketch code (MIDI library, potentiometers, etc).

mod mozzi_gen;

use std::path::PathBuf;

use clap::Parser;
use pdast::from_json;

#[derive(Parser, Debug)]
#[command(
    name = "pdast2mozzi",
    version,
    about = "Convert a pdast JSON AST to a Mozzi (Arduino) sketch",
    long_about = None,
)]
struct Args {
    /// Path to the JSON AST file (from pd2ast). Use '-' to read from stdin.
    ast: String,

    /// Write .ino output to FILE instead of stdout.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Suppress warnings.
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let args = Args::parse();

    let json = if args.ast == "-" {
        let mut s = String::new();
        use std::io::Read;
        std::io::stdin().read_to_string(&mut s).unwrap_or_else(|e| {
            eprintln!("Error reading stdin: {e}");
            std::process::exit(1);
        });
        s
    } else {
        std::fs::read_to_string(&args.ast).unwrap_or_else(|e| {
            eprintln!("Error reading {:?}: {e}", args.ast);
            std::process::exit(1);
        })
    };

    let patch = from_json(&json).unwrap_or_else(|e| {
        eprintln!("Error parsing JSON AST: {e}");
        std::process::exit(1);
    });

    let mut generator = mozzi_gen::MozziGenerator::new();
    let ino_code = generator.generate(&patch.root);

    if !args.quiet {
        for w in &generator.warnings {
            eprintln!("warning: {w}");
        }
    }

    match &args.output {
        Some(path) => {
            if let Err(e) = std::fs::write(path, &ino_code) {
                eprintln!("Error writing {:?}: {e}", path);
                std::process::exit(1);
            }
        }
        None => print!("{ino_code}"),
    }
}

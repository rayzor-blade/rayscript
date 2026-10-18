//! `rayscript-host [--root DIR]`: serves one extension on standard input and output. See the crate docs
//! for the protocol.

use rayscript_host::{serve, Config};
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut root: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--root" => match args.next() {
                Some(dir) => root = Some(PathBuf::from(dir)),
                None => {
                    eprintln!("rayscript-host: --root needs a folder");
                    return ExitCode::from(2);
                }
            },
            other => {
                eprintln!("rayscript-host: unknown argument `{other}`\nusage: rayscript-host [--root DIR]");
                return ExitCode::from(2);
            }
        }
    }
    // The interpreter nests Rust frames when a script calls back through a built-in: give it room.
    let run = std::thread::Builder::new().stack_size(64 << 20).spawn(move || serve(BufReader::new(io::stdin()), io::stdout().lock(), Config { root, ..Config::default() }));
    match run.map(|h| h.join()) {
        Ok(Ok(Ok(()))) => ExitCode::SUCCESS,
        Ok(Ok(Err(e))) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Ok(Ok(Err(e))) => {
            eprintln!("rayscript-host: {e}");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("rayscript-host: stopped unexpectedly");
            ExitCode::FAILURE
        }
    }
}

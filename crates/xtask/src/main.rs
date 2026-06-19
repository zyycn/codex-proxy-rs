mod build_web;
mod release;

use std::{
    error::Error,
    io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

type XtaskResult = Result<(), Box<dyn Error>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> XtaskResult {
    match std::env::args().nth(1).as_deref() {
        Some("build-web") => build_web::run(),
        Some("release") => release::run(),
        _ => {
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <build-web|release>");
}

pub(crate) fn repository_root() -> Result<PathBuf, io::Error> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("failed to derive repository root"))
}

pub(crate) fn run_command(program: &str, args: &[&str], current_dir: &Path) -> XtaskResult {
    eprintln!("running: {} {}", program, args.join(" "));
    let status = Command::new(program)
        .args(args)
        .current_dir(current_dir)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("command `{}` exited with status {status}", program)).into())
    }
}

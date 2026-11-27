mod build;
mod linker;
mod nixstore;
mod plan;
mod sha256;
mod tar_gz;
mod toml;

use std::{env, fs, os::unix::fs::symlink, path::Path, process};

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    if let Some(arg0) = args.first() {
        let base = Path::new(arg0)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if base == "drvrust-ld" {
            return linker::run(&args[1..]);
        }
    }
    let Some(cmd) = args.get(1).map(String::as_str) else {
        return Err("usage: drvrust-worker <internal-plan|internal-symlink|internal-mkdir|build-crate|vendor-crates|build-cargo>".into());
    };
    match cmd {
        "internal-plan" => plan::internal_plan(),
        "internal-symlink" => {
            let target = args.get(2).ok_or("internal-symlink: missing target")?;
            let out = args.get(3).ok_or("internal-symlink: missing out")?;
            symlink(target, out).map_err(|e| format!("symlinking {out} -> {target}: {e}"))
        }
        "internal-mkdir" => {
            let out = args.get(2).ok_or("internal-mkdir: missing out")?;
            fs::create_dir_all(out).map_err(|e| format!("mkdir {out}: {e}"))
        }
        "build-crate" => build::build_crate(),
        "vendor-crates" => build::vendor_crates(),
        "build-cargo" => build::build_cargo(),
        other => Err(format!("unknown drvrust-worker command: {other}")),
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("drvrust-worker error: {err}");
        process::exit(1);
    }
}

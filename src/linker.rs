//! Minimal GNU ld wrapper invoked by `rustc -C linker=.../drvrust-ld`.
//! Eliminates `gcc-15.3.0` (264 MiB NAR / 98.6 MiB compressed) from pure-Rust links.

use std::{env, process::Command};

pub fn run(raw_args: &[String]) -> Result<(), String> {
    let ld = env::var("DRVRUST_LD").map_err(|_| "DRVRUST_LD not set")?;
    let glibc = env::var("DRVRUST_GLIBC").map_err(|_| "DRVRUST_GLIBC not set")?;
    let gcc_lib = env::var("DRVRUST_GCC_LIB").map_err(|_| "DRVRUST_GCC_LIB not set")?;
    let crt = env::var("DRVRUST_CRT").map_err(|_| "DRVRUST_CRT not set")?;
    let arch = env::var("DRVRUST_ARCH").unwrap_or_else(|_| env::consts::ARCH.into());

    let (emulation, dyn_linker) = match arch.as_str() {
        "x86_64" => ("elf_x86_64", format!("{glibc}/lib/ld-linux-x86-64.so.2")),
        "aarch64" => ("aarch64linux", format!("{glibc}/lib/ld-linux-aarch64.so.1")),
        other => return Err(format!("unsupported linker arch: {other}")),
    };

    let mut is_shared = false;
    let mut is_static = false;
    let mut translated: Vec<String> = Vec::with_capacity(raw_args.len() + 16);

    let mut i = 0;
    while i < raw_args.len() {
        let a = &raw_args[i];
        i += 1;
        if a == "-shared" {
            is_shared = true;
            continue;
        }
        if a == "-static" {
            is_static = true;
            continue;
        }
        if let Some(rest) = a.strip_prefix("-Wl,") {
            for part in rest.split(',') {
                if !part.is_empty() {
                    translated.push(part.to_owned());
                }
            }
            continue;
        }
        // Ignore GCC-driver-only flags emitted by rustc.
        if matches!(
            a.as_str(),
            "-m64" | "-nodefaultlibs" | "-nostartfiles" | "-pie" | "-no-pie" | "-Wl"
        ) || a.starts_with("-B")
            || a.starts_with("-fuse-ld=")
            || a.starts_with("-m")
        {
            continue;
        }
        translated.push(a.clone());
    }

    let mut cmd = Command::new(&ld);
    cmd.args(["--eh-frame-hdr", "--build-id", "-m", emulation]);
    if is_shared {
        cmd.arg("-shared");
    } else if is_static {
        cmd.arg("-static");
    } else {
        cmd.arg("-pie");
        cmd.args(["-dynamic-linker", &dyn_linker]);
    }
    if !is_static {
        cmd.args(["-rpath", &format!("{glibc}/lib:{gcc_lib}/lib")]);
    }
    if !is_shared {
        cmd.arg(if is_static {
            format!("{glibc}/lib/crt1.o")
        } else {
            format!("{glibc}/lib/Scrt1.o")
        });
    }
    cmd.arg(format!("{glibc}/lib/crti.o"));
    cmd.arg(if is_static {
        format!("{crt}/crtbeginT.o")
    } else {
        format!("{crt}/crtbeginS.o")
    });

    cmd.args(&translated);

    cmd.arg(format!("-L{glibc}/lib"));
    cmd.arg(format!("-L{gcc_lib}/lib"));
    cmd.arg(format!("-L{crt}"));
    if is_static {
        cmd.args(["--start-group", "-lgcc", "-lgcc_eh", "-lc", "--end-group"]);
    } else {
        cmd.args(["-lgcc_s", "-lc", "-lgcc"]);
    }
    cmd.arg(if is_static {
        format!("{crt}/crtend.o")
    } else {
        format!("{crt}/crtendS.o")
    });
    cmd.arg(format!("{glibc}/lib/crtn.o"));

    let status = cmd
        .status()
        .map_err(|e| format!("invoking {ld}: {e}"))?;
    if !status.success() {
        return Err(format!("linker {ld} failed with {status}"));
    }
    Ok(())
}

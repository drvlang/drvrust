//! Crate compilation (Tier 1 direct rustc) and vendored Cargo build (Tier 2).

use crate::{
    tar_gz,
    toml::{self, Value},
};
use std::{
    collections::{BTreeSet, VecDeque},
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

fn env_req(k: &str) -> Result<String, String> {
    env::var(k).map_err(|_| format!("missing env {k}"))
}

fn env_opt(k: &str) -> Option<String> {
    env::var(k).ok().filter(|s| !s.is_empty())
}

fn expand_features(manifest: &Value, use_default: bool, requested: &[String]) -> BTreeSet<String> {
    let mut active = BTreeSet::new();
    let Some(table) = manifest.get("features").and_then(Value::as_table) else {
        return active;
    };
    let mut queue = VecDeque::new();
    if use_default && table.contains_key("default") {
        queue.push_back("default".to_owned());
    }
    for r in requested {
        if !r.is_empty() {
            queue.push_back(r.clone());
        }
    }
    while let Some(feat) = queue.pop_front() {
        if !active.insert(feat.clone()) {
            continue;
        }
        if let Some(items) = table.get(&feat).and_then(Value::as_array) {
            for item in items {
                if let Some(s) = item.as_str() {
                    if !s.contains('/') && !s.starts_with("dep:") && table.contains_key(s) {
                        queue.push_back(s.to_owned());
                    }
                }
            }
        }
    }
    active
}

fn crate_has_unit_tests(dir: &Path) -> bool {
    fn walk(p: &Path) -> bool {
        let Ok(entries) = fs::read_dir(p) else {
            return false;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                if walk(&path) {
                    return true;
                }
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(text) = fs::read_to_string(&path) {
                    if text.contains("#[test]")
                        || text.contains("#[cfg(test)]")
                        || text.contains("macro_rules!")
                        || text.contains("include!")
                    {
                        return true;
                    }
                }
            }
        }
        false
    }
    walk(&dir.join("src"))
}

fn apply_cargo_pkg_env(cmd: &mut Command, name: &str, version: &str, dir: &Path) {
    let parts: Vec<&str> = version.split(['.', '-', '+']).collect();
    cmd.env("CARGO_PKG_NAME", name)
        .env("CARGO_PKG_VERSION", version)
        .env("CARGO_PKG_VERSION_MAJOR", parts.first().copied().unwrap_or("0"))
        .env("CARGO_PKG_VERSION_MINOR", parts.get(1).copied().unwrap_or("0"))
        .env("CARGO_PKG_VERSION_PATCH", parts.get(2).copied().unwrap_or("0"))
        .env("CARGO_PKG_VERSION_PRE", "")
        .env("CARGO_MANIFEST_DIR", dir)
        .env("CARGO_CRATE_NAME", name.replace('-', "_"));
}

fn parse_externs(raw: &str) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if let Some((alias, path)) = line.split_once('=') {
            out.push((alias.to_owned(), PathBuf::from(path)));
        }
    }
    out
}

pub fn build_crate() -> Result<(), String> {
    let out = PathBuf::from(env_req("out")?);
    let rustc = env_req("RUSTC")?;
    let pkg_name = env_req("CRATE_NAME")?;
    let pkg_version = env_req("CRATE_VERSION")?;
    let is_target = env_opt("IS_TARGET").as_deref() == Some("1");
    let test_only = env_opt("TEST_ONLY").as_deref() == Some("1");
    let use_default = env_opt("DEFAULT_FEATURES").as_deref() != Some("0");
    let requested: Vec<String> = env_opt("FEATURES")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let externs = parse_externs(&env_opt("DRVRUST_EXTERNS").unwrap_or_default());

    let tmp = env_opt("TMPDIR").unwrap_or_else(|| "/tmp".into());
    let work_dir = PathBuf::from(&tmp).join("work");
    fs::create_dir_all(&work_dir).map_err(|e| format!("mkdir work: {e}"))?;

    let crate_dir = if let Some(archive) = env_opt("CRATE_ARCHIVE") {
        let unpacked = work_dir.join("crate");
        tar_gz::extract_crate(Path::new(&archive), &unpacked, false)?;
        unpacked
    } else {
        PathBuf::from(env_req("CRATE_DIR")?)
    };

    let manifest_text = fs::read_to_string(crate_dir.join("Cargo.toml"))
        .map_err(|e| format!("reading Cargo.toml: {e}"))?;
    let manifest = toml::parse(&manifest_text)?;
    let edition = manifest
        .get("package")
        .and_then(|p| p.get("edition"))
        .and_then(Value::as_str)
        .unwrap_or("2015");
    let features = expand_features(&manifest, use_default, &requested);
    let crate_ident = pkg_name.replace('-', "_");
    let metadata = format!("{pkg_name}-{pkg_version}");

    let lib_path = manifest
        .get("lib")
        .and_then(|l| l.get("path"))
        .and_then(Value::as_str)
        .map(|p| crate_dir.join(p))
        .unwrap_or_else(|| crate_dir.join("src/lib.rs"));
    let has_lib = lib_path.is_file();

    let mut bins: Vec<(String, PathBuf)> = Vec::new();
    if let Some(bin_arr) = manifest.get("bin").and_then(Value::as_array) {
        for b in bin_arr {
            if let Some(bname) = b.get("name").and_then(Value::as_str) {
                let bpath = b
                    .get("path")
                    .and_then(Value::as_str)
                    .map(|p| crate_dir.join(p))
                    .unwrap_or_else(|| crate_dir.join(format!("src/bin/{bname}.rs")));
                if bpath.is_file() {
                    bins.push((bname.to_owned(), bpath));
                }
            }
        }
    } else {
        if crate_dir.join("src/main.rs").is_file() {
            bins.push((pkg_name.clone(), crate_dir.join("src/main.rs")));
        }
        if let Ok(entries) = fs::read_dir(crate_dir.join("src/bin")) {
            let mut extra: Vec<(String, PathBuf)> = entries
                .flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                        let stem = p.file_stem()?.to_str()?.to_owned();
                        Some((stem, p))
                    } else {
                        None
                    }
                })
                .collect();
            extra.sort_by(|a, b| a.0.cmp(&b.0));
            bins.extend(extra);
        }
    }

    fs::create_dir_all(&out).map_err(|e| format!("mkdir out: {e}"))?;

    let mut dep_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for (_, path) in &externs {
        if let Some(parent) = path.parent() {
            dep_dirs.insert(parent.to_path_buf());
        }
    }

    let add_common = |cmd: &mut Command| {
        cmd.arg("--edition").arg(edition);
        cmd.arg("-C").arg(format!("metadata={metadata}"));
        cmd.arg("--remap-path-prefix")
            .arg(format!("{}=/source", crate_dir.display()));
        if let Some(src_root) = env_opt("SRC_ROOT") {
            cmd.arg("--remap-path-prefix")
                .arg(format!("{src_root}=/source"));
        }
        cmd.arg("--remap-path-prefix")
            .arg(format!("{}=/build", work_dir.display()));
        if !is_target {
            cmd.arg("--cap-lints").arg("allow");
        }
        for f in &features {
            cmd.arg("--cfg").arg(format!("feature=\"{f}\""));
        }
        for d in &dep_dirs {
            cmd.arg("-L").arg(format!("dependency={}", d.display()));
        }
        for (alias, path) in &externs {
            cmd.arg("--extern")
                .arg(format!("{alias}={}", path.display()));
        }
        apply_cargo_pkg_env(cmd, &pkg_name, &pkg_version, &crate_dir);
    };

    if !is_target {
        if !has_lib {
            return Err(format!("dependency crate {pkg_name} missing lib target"));
        }
        let lib_out = out.join(format!("lib{crate_ident}.rlib"));
        let mut cmd = Command::new(&rustc);
        cmd.args(["--crate-name", &crate_ident, "--crate-type", "lib", "-C", "opt-level=3"]);
        add_common(&mut cmd);
        cmd.arg(&lib_path).arg("-o").arg(&lib_out);
        let status = cmd.status().map_err(|e| format!("running rustc: {e}"))?;
        if !status.success() {
            return Err(format!("rustc failed building {pkg_name}: {status}"));
        }
        return Ok(());
    }

    // Target crate (package or workspace member).
    let linker = env_req("DRVRUST_LINKER")?;
    let mut local_lib: Option<PathBuf> = None;

    if has_lib {
        let target_dir = if bins.is_empty() && !test_only {
            out.clone()
        } else {
            work_dir.clone()
        };
        let lib_out = target_dir.join(format!("lib{crate_ident}.rlib"));
        let mut cmd = Command::new(&rustc);
        cmd.args(["--crate-name", &crate_ident, "--crate-type", "lib", "-C", "opt-level=3"]);
        add_common(&mut cmd);
        cmd.arg(&lib_path).arg("-o").arg(&lib_out);
        let status = cmd.status().map_err(|e| format!("running rustc: {e}"))?;
        if !status.success() {
            return Err(format!("rustc failed building lib {pkg_name}: {status}"));
        }
        local_lib = Some(lib_out);
    }

    let run_tests = test_only || crate_has_unit_tests(&crate_dir);

    if !test_only && !bins.is_empty() {
        let bin_dir = out.join("bin");
        fs::create_dir_all(&bin_dir).map_err(|e| format!("mkdir bin: {e}"))?;
        for (bname, bpath) in &bins {
            let bcrate = bname.replace('-', "_");
            let mut cmd = Command::new(&rustc);
            cmd.args(["--crate-name", &bcrate, "--crate-type", "bin", "-C", "opt-level=3"]);
            cmd.arg("-C").arg(format!("linker={linker}"));
            add_common(&mut cmd);
            if let Some(lib_rlib) = &local_lib {
                cmd.arg("--extern")
                    .arg(format!("{crate_ident}={}", lib_rlib.display()));
            }
            cmd.arg(bpath).arg("-o").arg(bin_dir.join(bname));
            let status = cmd.status().map_err(|e| format!("running rustc: {e}"))?;
            if !status.success() {
                return Err(format!("rustc failed building bin {bname}: {status}"));
            }
        }
    }

    if run_tests {
        let test_bin_dir = work_dir.join("tests");
        fs::create_dir_all(&test_bin_dir).map_err(|e| format!("mkdir tests: {e}"))?;
        if has_lib {
            let tbin = test_bin_dir.join("lib_test");
            let mut cmd = Command::new(&rustc);
            cmd.args(["--crate-name", &crate_ident, "--test", "-C", "opt-level=0"]);
            cmd.arg("-C").arg(format!("linker={linker}"));
            add_common(&mut cmd);
            cmd.arg(&lib_path).arg("-o").arg(&tbin);
            let status = cmd.status().map_err(|e| format!("rustc --test lib: {e}"))?;
            if !status.success() {
                return Err(format!("rustc --test failed for lib {pkg_name}: {status}"));
            }
            let status = Command::new(&tbin)
                .status()
                .map_err(|e| format!("running test {}: {e}", tbin.display()))?;
            if !status.success() {
                return Err(format!("tests failed for lib {pkg_name}: {status}"));
            }
        }
        for (bname, bpath) in &bins {
            let bcrate = bname.replace('-', "_");
            let tbin = test_bin_dir.join(format!("bin_{bcrate}_test"));
            let mut cmd = Command::new(&rustc);
            cmd.args(["--crate-name", &bcrate, "--test", "-C", "opt-level=0"]);
            cmd.arg("-C").arg(format!("linker={linker}"));
            add_common(&mut cmd);
            if let Some(lib_rlib) = &local_lib {
                cmd.arg("--extern")
                    .arg(format!("{crate_ident}={}", lib_rlib.display()));
            }
            cmd.arg(bpath).arg("-o").arg(&tbin);
            let status = cmd.status().map_err(|e| format!("rustc --test bin: {e}"))?;
            if !status.success() {
                return Err(format!("rustc --test failed for bin {bname}: {status}"));
            }
            let status = Command::new(&tbin)
                .status()
                .map_err(|e| format!("running test {}: {e}", tbin.display()))?;
            if !status.success() {
                return Err(format!("tests failed for bin {bname}: {status}"));
            }
        }
    }

    Ok(())
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn vendor_crates() -> Result<(), String> {
    let out = PathBuf::from(env_req("out")?);
    let list_path = env_req("crateListPath")?;
    fs::create_dir_all(&out).map_err(|e| format!("mkdir out: {e}"))?;

    let list = fs::read_to_string(&list_path).map_err(|e| format!("reading crateList: {e}"))?;
    let mut jobs: Vec<(String, String, String, PathBuf)> = Vec::new();
    for line in list.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 4 {
            return Err(format!("malformed crate list line: {line}"));
        }
        jobs.push((
            parts[0].to_owned(),
            parts[1].to_owned(),
            parts[2].to_owned(),
            PathBuf::from(parts[3]),
        ));
    }

    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let workers = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 8);

    thread::scope(|s| {
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let error = Arc::clone(&error);
            let out = &out;
            s.spawn(move || loop {
                if error.lock().unwrap().is_some() {
                    break;
                }
                let next = queue.lock().unwrap().pop_front();
                let Some((name, version, checksum, archive)) = next else {
                    break;
                };
                let dest = out.join(format!("{name}-{version}"));
                let res = (|| -> Result<(), String> {
                    let hashes = tar_gz::extract_crate(&archive, &dest, true)?;
                    let mut json = String::from("{\"files\":{");
                    for (i, (k, v)) in hashes.iter().enumerate() {
                        if i > 0 {
                            json.push(',');
                        }
                        json.push_str(&json_escape(k));
                        json.push(':');
                        json.push_str(&json_escape(v));
                    }
                    json.push_str("},\"package\":");
                    json.push_str(&json_escape(&checksum));
                    json.push('}');
                    fs::write(dest.join(".cargo-checksum.json"), json)
                        .map_err(|e| format!("writing checksum json: {e}"))?;
                    Ok(())
                })();
                if let Err(e) = res {
                    *error.lock().unwrap() = Some(e);
                    break;
                }
            });
        }
    });
    if let Some(e) = error.lock().unwrap().take() {
        return Err(e);
    }
    Ok(())
}

fn copy_dir_writable(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("mkdir {}: {e}", dst.display()))?;
    for entry in fs::read_dir(src).map_err(|e| format!("readdir {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| format!("dirent: {e}"))?;
        let ftype = entry.file_type().map_err(|e| format!("filetype: {e}"))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ftype.is_dir() {
            copy_dir_writable(&from, &to)?;
        } else if ftype.is_symlink() {
            let link = fs::read_link(&from).map_err(|e| format!("readlink: {e}"))?;
            std::os::unix::fs::symlink(link, &to).map_err(|e| format!("symlink: {e}"))?;
        } else {
            fs::copy(&from, &to).map_err(|e| format!("copy {}: {e}", from.display()))?;
            let mode = fs::metadata(&to)
                .map_err(|e| format!("stat: {e}"))?
                .permissions()
                .mode();
            let _ = fs::set_permissions(&to, fs::Permissions::from_mode(mode | 0o200));
        }
    }
    Ok(())
}

pub fn build_cargo() -> Result<(), String> {
    let out = PathBuf::from(env_req("out")?);
    let rustc = env_req("RUSTC")?;
    let cargo = env_req("CARGO")?;
    let stdenv = env_req("STDENV")?;
    let src = PathBuf::from(env_req("DRVRUST_SRC")?);
    let root_rel = env_opt("DRVRUST_ROOT_REL").unwrap_or_default();
    let vendor = env_req("DRVRUST_VENDOR")?;
    let selected = env_opt("DRVRUST_PACKAGE");
    let filtered_test = env_opt("DRVRUST_FILTERED_TEST").as_deref() == Some("1");
    let test_args = env_opt("DRVRUST_TEST_ARGS").unwrap_or_default();
    let native_inputs = env_opt("DRVRUST_NATIVE_BUILD_INPUTS").unwrap_or_default();
    let build_inputs = env_opt("DRVRUST_BUILD_INPUTS").unwrap_or_default();

    let tmp = PathBuf::from(env_opt("TMPDIR").unwrap_or_else(|| "/tmp".into()));
    let work_src = tmp.join("src");
    copy_dir_writable(&src, &work_src)?;
    let cargo_root = if root_rel.is_empty() {
        work_src.clone()
    } else {
        work_src.join(&root_rel)
    };

    let cargo_home = tmp.join("cargo-home");
    fs::create_dir_all(&cargo_home).map_err(|e| format!("mkdir cargo-home: {e}"))?;
    fs::write(
        cargo_home.join("config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"{}\"\n",
            vendor
        ),
    )
    .map_err(|e| format!("writing cargo config.toml: {e}"))?;

    let target_dir = tmp.join("target");
    let jobs = env_opt("NIX_BUILD_CORES").unwrap_or_else(|| "4".into());
    let rustflags = format!(
        "--remap-path-prefix={}=/build --remap-path-prefix={}=/vendor",
        tmp.display(),
        vendor
    );

    // Source stdenv/setup so C compiler, linker, and any nativeBuildInputs/buildInputs
    // setup hooks (e.g. pkg-config) are configured identically to buildRustPackage.
    let mut script = String::new();
    script.push_str("set -euo pipefail\n");
    script.push_str(&format!("export buildInputs=\"{build_inputs}\"\n"));
    script.push_str(&format!("export nativeBuildInputs=\"{native_inputs}\"\n"));
    script.push_str(&format!("source \"{stdenv}/setup\"\n"));
    script.push_str(&format!("cd \"{}\"\n", cargo_root.display()));
    script.push_str(&format!("export RUSTC=\"{rustc}\"\n"));
    script.push_str(&format!("export CARGO_HOME=\"{}\"\n", cargo_home.display()));
    script.push_str(&format!("export CARGO_TARGET_DIR=\"{}\"\n", target_dir.display()));
    script.push_str(&format!("export CARGO_BUILD_JOBS=\"{jobs}\"\n"));
    script.push_str(&format!("export RUSTFLAGS=\"{rustflags}\"\n"));

    let mut pkg_flag = String::new();
    if let Some(pkg) = &selected {
        pkg_flag = format!(" --package '{}'", pkg.replace('\'', "'\\''"));
    }
    if !filtered_test {
        script.push_str(&format!(
            "\"{cargo}\" build --release --offline --locked{pkg_flag}\n"
        ));
    }
    let mut test_cmd = format!("\"{cargo}\" test --release --offline --locked{pkg_flag}");
    if !test_args.is_empty() {
        test_cmd.push_str(" --");
        for arg in test_args.lines() {
            if !arg.is_empty() {
                test_cmd.push_str(&format!(" '{}'", arg.replace('\'', "'\\''")));
            }
        }
    }
    test_cmd.push('\n');
    script.push_str(&test_cmd);

    let bash = format!("{stdenv}/setup");
    // Locate bash from stdenv's SHELL or PATH via stdenv/setup's interpreter.
    let setup_text = fs::read_to_string(&bash).map_err(|e| format!("reading {bash}: {e}"))?;
    let shell = setup_text
        .lines()
        .find_map(|l| l.strip_prefix("export SHELL="))
        .map(|s| s.trim_matches(['"', '\'']))
        .unwrap_or("/bin/sh")
        .to_owned();

    let status = Command::new(&shell)
        .args(["-e", "-c", &script])
        .status()
        .map_err(|e| format!("running {shell}: {e}"))?;
    if !status.success() {
        return Err(format!("cargo build/test failed: {status}"));
    }

    fs::create_dir_all(&out).map_err(|e| format!("mkdir out: {e}"))?;
    if !filtered_test {
        let rel_dir = target_dir.join("release");
        let bin_dir = out.join("bin");
        let mut installed = false;
        if let Ok(entries) = fs::read_dir(&rel_dir) {
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_file() {
                    continue;
                }
                let name = e.file_name();
                let Some(name_str) = name.to_str() else {
                    continue;
                };
                if name_str.ends_with(".d")
                    || name_str.ends_with(".rlib")
                    || name_str.ends_with(".rmeta")
                    || name_str.ends_with(".so")
                    || name_str.ends_with(".a")
                    || name_str.starts_with('.')
                {
                    continue;
                }
                let mode = fs::metadata(&path)
                    .map_err(|e| format!("stat: {e}"))?
                    .permissions()
                    .mode();
                if mode & 0o111 != 0 {
                    fs::create_dir_all(&bin_dir).map_err(|e| format!("mkdir bin: {e}"))?;
                    let dest = bin_dir.join(name_str);
                    fs::copy(&path, &dest).map_err(|e| format!("copy bin: {e}"))?;
                    let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(mode & 0o777));
                    installed = true;
                }
            }
        }
        let _ = installed;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_closure_expands_hex_defaults() {
        let m = toml::parse(
            r#"
[features]
alloc = []
default = ["std"]
std = ["alloc"]
"#,
        )
        .unwrap();
        let active = expand_features(&m, true, &[]);
        assert!(active.contains("default"));
        assert!(active.contains("std"));
        assert!(active.contains("alloc"));
    }
}

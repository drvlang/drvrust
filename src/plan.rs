//! Direct Nix daemon ATerm planner for Cargo v4 lockfiles.
//! Emits either Tier 1 (per-crate direct rustc + drvrust-ld) or Tier 2
//! (parallel Rust crate vendoring + Cargo with rustcSlim).

use crate::{
    nixstore::{
        Derivation, DerivationOutput, TextNode, downstream_placeholder, hash_placeholder,
        make_fetchurl_drv, make_text_path, register_graph, store_dir,
    },
    toml::{self, Value},
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
struct LockPkg {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    dependencies: Vec<String>,
}

#[derive(Clone, Debug)]
struct DepSpec {
    alias: String,
    package: String,
    default_features: bool,
    features: Vec<String>,
}

fn ca_outputs() -> Vec<DerivationOutput> {
    vec![DerivationOutput {
        name: "out".into(),
        path: String::new(),
        hash_algo: "r:sha256".into(),
        hash: String::new(),
    }]
}

fn parse_lock(root: &Path) -> Result<Vec<LockPkg>, String> {
    let lock_path = root.join("Cargo.lock");
    let text = fs::read_to_string(&lock_path)
        .map_err(|e| format!("reading {}: {e}", lock_path.display()))?;
    let doc = toml::parse(&text)?;
    if doc.get("version").and_then(Value::as_int) != Some(4) {
        return Err("drvrust requires Cargo.lock version 4".into());
    }
    let pkgs = doc
        .get("package")
        .and_then(Value::as_array)
        .ok_or("Cargo.lock missing [[package]] entries")?;
    let mut out = Vec::with_capacity(pkgs.len());
    for p in pkgs {
        let name = p
            .get("name")
            .and_then(Value::as_str)
            .ok_or("Cargo.lock package missing name")?
            .to_owned();
        let version = p
            .get("version")
            .and_then(Value::as_str)
            .ok_or("Cargo.lock package missing version")?
            .to_owned();
        let source = p.get("source").and_then(Value::as_str).map(str::to_owned);
        let checksum = p.get("checksum").and_then(Value::as_str).map(str::to_owned);
        if let Some(src) = &source {
            if src != "registry+https://github.com/rust-lang/crates.io-index" || checksum.is_none()
            {
                return Err(format!(
                    "drvrust supports checksum-locked crates.io and workspace packages (unsupported: {name} {version})"
                ));
            }
        }
        let dependencies = p
            .get("dependencies")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        out.push(LockPkg {
            name,
            version,
            source,
            checksum,
            dependencies,
        });
    }
    Ok(out)
}

fn match_glob(pattern: &str, name: &str) -> bool {
    let p = pattern.as_bytes();
    let s = name.as_bytes();
    let (mut pi, mut si) = (0usize, 0usize);
    let (mut star, mut match_i) = (None, 0usize);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            match_i = si;
            pi += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            match_i += 1;
            si = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

fn expand_member_pattern(root: &Path, pat: &str) -> Vec<PathBuf> {
    let parts: Vec<&str> = pat.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    let mut cur = vec![PathBuf::new()];
    for part in parts {
        let mut next = Vec::new();
        if part.contains('*') || part.contains('?') {
            for base in &cur {
                let dir = if base.as_os_str().is_empty() {
                    root.to_path_buf()
                } else {
                    root.join(base)
                };
                if let Ok(entries) = fs::read_dir(&dir) {
                    let mut names: Vec<String> = entries
                        .flatten()
                        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                        .filter(|n| match_glob(part, n))
                        .collect();
                    names.sort();
                    for n in names {
                        let cand = base.join(&n);
                        if root.join(&cand).is_dir() {
                            next.push(cand);
                        }
                    }
                }
            }
        } else {
            for base in &cur {
                let cand = base.join(part);
                if root.join(&cand).is_dir() {
                    next.push(cand);
                }
            }
        }
        cur = next;
    }
    cur.into_iter()
        .filter(|rel| root.join(rel).join("Cargo.toml").is_file())
        .collect()
}

fn discover_workspace_members(root: &Path, root_manifest: &Value) -> BTreeMap<String, PathBuf> {
    let mut members = BTreeMap::new();
    let mut dirs = Vec::new();
    if root_manifest.get("package").is_some() {
        dirs.push(PathBuf::new());
    }
    if let Some(arr) = root_manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(Value::as_array)
    {
        for item in arr {
            if let Some(pat) = item.as_str() {
                dirs.extend(expand_member_pattern(root, pat));
            }
        }
    }
    for rel in dirs {
        let mpath = if rel.as_os_str().is_empty() {
            root.join("Cargo.toml")
        } else {
            root.join(&rel).join("Cargo.toml")
        };
        if let Ok(text) = fs::read_to_string(&mpath) {
            if let Ok(doc) = toml::parse(&text) {
                if let Some(name) = doc
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                {
                    members.insert(name.to_owned(), rel);
                }
            }
        }
    }
    members
}

fn parse_dep_specs(manifest: &Value) -> Vec<DepSpec> {
    let mut specs = Vec::new();
    let Some(deps) = manifest.get("dependencies").and_then(Value::as_table) else {
        return specs;
    };
    for (alias, val) in deps {
        match val {
            Value::Str(_) => specs.push(DepSpec {
                alias: alias.clone(),
                package: alias.clone(),
                default_features: true,
                features: Vec::new(),
            }),
            Value::Table(t) => {
                let package = t
                    .get("package")
                    .and_then(Value::as_str)
                    .unwrap_or(alias)
                    .to_owned();
                let default_features = t
                    .get("default-features")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                let features = t
                    .get("features")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                specs.push(DepSpec {
                    alias: alias.clone(),
                    package,
                    default_features,
                    features,
                });
            }
            _ => {}
        }
    }
    specs
}

fn resolve_dep_idx(pkgs: &[LockPkg], spec: &str) -> Result<usize, String> {
    let mut parts = spec.split_whitespace();
    let name = parts.next().ok_or("empty dependency entry in Cargo.lock")?;
    let ver = parts.next();
    let matches: Vec<usize> = pkgs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.name == name && ver.is_none_or(|v| p.version == v))
        .map(|(i, _)| i)
        .collect();
    if matches.len() == 1 {
        Ok(matches[0])
    } else if matches.is_empty() {
        Err(format!("Cargo.lock dependency not found: {spec}"))
    } else {
        Err(format!("ambiguous Cargo.lock dependency: {spec}"))
    }
}

fn can_use_tier1(
    root: &Path,
    pkgs: &[LockPkg],
    members: &BTreeMap<String, PathBuf>,
    target_idx: usize,
    has_native_inputs: bool,
    has_test_args: bool,
) -> bool {
    if has_native_inputs || has_test_args {
        return false;
    }
    // BFS over reachable packages from target_idx.
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    visited.insert(target_idx);
    queue.push_back(target_idx);
    let mut external_count = 0usize;

    while let Some(idx) = queue.pop_front() {
        let p = &pkgs[idx];
        if p.source.is_some() {
            external_count += 1;
            // Only use Tier 1 when crates.io dependencies are simple leaf crates
            // (no transitive crates.io deps, avoiding hidden build.rs/proc-macro
            // feature unification in large graphs like hexyl).
            if !p.dependencies.is_empty() || external_count > 4 {
                return false;
            }
        } else {
            let Some(rel) = members.get(&p.name) else {
                return false;
            };
            let dir = if rel.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(rel)
            };
            if dir.join("build.rs").exists() || dir.join("tests").is_dir() {
                return false;
            }
            let Ok(text) = fs::read_to_string(dir.join("Cargo.toml")) else {
                return false;
            };
            let Ok(manifest) = toml::parse(&text) else {
                return false;
            };
            if manifest.get("dev-dependencies").is_some()
                || manifest.get("build-dependencies").is_some()
                || manifest.get("target").is_some()
            {
                return false;
            }
            if let Some(pkg) = manifest.get("package") {
                if pkg.get("build").is_some() {
                    return false;
                }
            }
            if let Some(lib) = manifest.get("lib") {
                if lib.get("proc-macro").and_then(Value::as_bool) == Some(true) {
                    return false;
                }
            }
            if let Some(deps) = manifest.get("dependencies").and_then(Value::as_table) {
                for v in deps.values() {
                    if let Some(t) = v.as_table() {
                        if t.get("workspace").and_then(Value::as_bool) == Some(true)
                            || t.get("optional").and_then(Value::as_bool) == Some(true)
                        {
                            return false;
                        }
                    }
                }
            }
        }
        for dep in &p.dependencies {
            let Ok(dep_idx) = resolve_dep_idx(pkgs, dep) else {
                return false;
            };
            if visited.insert(dep_idx) {
                queue.push_back(dep_idx);
            }
        }
    }
    true
}

fn env_req(k: &str) -> Result<String, String> {
    env::var(k).map_err(|_| format!("internal-plan: missing env {k}"))
}

fn env_opt(k: &str) -> Option<String> {
    env::var(k).ok().filter(|s| !s.is_empty())
}

pub fn internal_plan() -> Result<(), String> {
    let out_file = env_req("out")?;
    let system = env_req("system")?;
    let remote = env_req("NIX_REMOTE")?;
    let src = env_req("src")?;
    let member_path = env_req("memberPath")?;
    let package_arg = env_opt("package");
    let filtered_test = env_opt("filteredTest").as_deref() == Some("1");
    let test_args = env_opt("testArgs").unwrap_or_default();
    let native_build_inputs = env_opt("nativeBuildInputs").unwrap_or_default();
    let build_inputs = env_opt("buildInputs").unwrap_or_default();

    let worker = env_req("workerOut")?;
    let rustc_slim = env_req("rustcSlimOut")?;
    let crt_slice = env_req("crtSliceOut")?;
    let binutils = env_req("binutilsOut")?;
    let glibc = env_req("glibcOut")?;
    let gcc_lib = env_req("gccLibOut")?;
    let cargo_out = env_opt("cargoOut");
    let stdenv_out = env_opt("stdenvOut");

    let store = store_dir();
    let src_path = Path::new(&src);
    let root_path = if src_path.join("Cargo.lock").is_file() {
        src_path.to_path_buf()
    } else {
        src_path.join(&member_path)
    };
    let root_rel = root_path
        .strip_prefix(src_path)
        .unwrap_or(Path::new(""))
        .to_path_buf();

    let lock_pkgs = parse_lock(&root_path)?;
    let root_manifest_text = fs::read_to_string(root_path.join("Cargo.toml"))
        .map_err(|e| format!("reading Cargo.toml: {e}"))?;
    let root_manifest = toml::parse(&root_manifest_text)?;
    let members = discover_workspace_members(&root_path, &root_manifest);

    let member_manifest_path = if member_path == "." {
        src_path.join("Cargo.toml")
    } else {
        src_path.join(&member_path).join("Cargo.toml")
    };
    let member_manifest_text = fs::read_to_string(&member_manifest_path)
        .map_err(|e| format!("reading {}: {e}", member_manifest_path.display()))?;
    let member_manifest = toml::parse(&member_manifest_text)?;

    let selected: Option<String> = if let Some(pkg) = package_arg {
        Some(pkg)
    } else if member_path != "." {
        member_manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        member_manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };

    let has_native_inputs = !native_build_inputs.is_empty() || !build_inputs.is_empty();
    let has_test_args = !test_args.is_empty();

    let target_idx = selected.as_deref().and_then(|sel| {
        lock_pkgs
            .iter()
            .position(|p| p.source.is_none() && p.name == sel)
    });

    let use_tier1 = target_idx.is_some_and(|idx| {
        can_use_tier1(
            &root_path,
            &lock_pkgs,
            &members,
            idx,
            has_native_inputs,
            has_test_args,
        )
    });

    if use_tier1 {
        let target_idx = target_idx.unwrap();
        let (nodes, final_aterm) = plan_tier1(
            &store,
            &system,
            &src,
            &root_path,
            &root_rel,
            &lock_pkgs,
            &members,
            target_idx,
            filtered_test,
            &worker,
            &rustc_slim,
            &crt_slice,
            &binutils,
            &glibc,
            &gcc_lib,
        )?;
        register_graph(&remote, &nodes)?;
        fs::write(&out_file, &final_aterm)
            .map_err(|e| format!("writing final derivation {out_file}: {e}"))?;
        return Ok(());
    }

    // Tier 2: Cargo with rustcSlim + parallel Rust crate vendoring.
    let cargo = cargo_out.ok_or("internal-plan: Tier 2 requires cargoOut")?;
    let stdenv = stdenv_out.ok_or("internal-plan: Tier 2 requires stdenvOut")?;
    let pname = selected.clone().unwrap_or_else(|| {
        member_manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("cargo-workspace")
            .to_owned()
    });
    let version = member_manifest
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(Value::as_str)
        .or_else(|| {
            root_manifest
                .get("workspace")
                .and_then(|w| w.get("package"))
                .and_then(|p| p.get("version"))
                .and_then(Value::as_str)
        })
        .unwrap_or("workspace")
        .to_owned();

    let (nodes, final_aterm) = plan_tier2(
        &store,
        &system,
        &src,
        &root_rel,
        &pname,
        &version,
        selected.as_deref(),
        filtered_test,
        &test_args,
        &native_build_inputs,
        &build_inputs,
        &lock_pkgs,
        &worker,
        &rustc_slim,
        &cargo,
        &stdenv,
    )?;
    register_graph(&remote, &nodes)?;
    fs::write(&out_file, &final_aterm)
        .map_err(|e| format!("writing final derivation {out_file}: {e}"))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn plan_tier1(
    store: &str,
    system: &str,
    src: &str,
    root_path: &Path,
    root_rel: &Path,
    lock_pkgs: &[LockPkg],
    members: &BTreeMap<String, PathBuf>,
    target_idx: usize,
    filtered_test: bool,
    worker: &str,
    rustc_slim: &str,
    crt_slice: &str,
    binutils: &str,
    glibc: &str,
    gcc_lib: &str,
) -> Result<(Vec<TextNode>, Vec<u8>), String> {
    let builder = format!("{worker}/bin/drvrust-worker");
    let out_placeholder = hash_placeholder("out");

    // Collect post-order topological sort of reachable packages.
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    fn dfs(
        idx: usize,
        pkgs: &[LockPkg],
        visited: &mut BTreeSet<usize>,
        order: &mut Vec<usize>,
    ) -> Result<(), String> {
        if !visited.insert(idx) {
            return Ok(());
        }
        for dep in &pkgs[idx].dependencies {
            let d = resolve_dep_idx(pkgs, dep)?;
            dfs(d, pkgs, visited, order)?;
        }
        order.push(idx);
        Ok(())
    }
    dfs(target_idx, lock_pkgs, &mut visited, &mut order)?;

    // Gather feature requests from workspace dependents.
    let mut dep_specs_by_pkg: BTreeMap<usize, Vec<DepSpec>> = BTreeMap::new();
    let mut requested_features: BTreeMap<usize, (bool, BTreeSet<String>)> = BTreeMap::new();
    for &idx in &order {
        let p = &lock_pkgs[idx];
        if p.source.is_none() {
            let rel = &members[&p.name];
            let mpath = if rel.as_os_str().is_empty() {
                root_path.join("Cargo.toml")
            } else {
                root_path.join(rel).join("Cargo.toml")
            };
            let manifest = toml::parse(&fs::read_to_string(&mpath).map_err(|e| format!("{e}"))?)?;
            let specs = parse_dep_specs(&manifest);
            for sp in &specs {
                if let Some(dep_idx) = p
                    .dependencies
                    .iter()
                    .filter_map(|d| resolve_dep_idx(lock_pkgs, d).ok())
                    .find(|&di| lock_pkgs[di].name == sp.package)
                {
                    let entry = requested_features
                        .entry(dep_idx)
                        .or_insert_with(|| (false, BTreeSet::new()));
                    if sp.default_features {
                        entry.0 = true;
                    }
                    for f in &sp.features {
                        entry.1.insert(f.clone());
                    }
                }
            }
            dep_specs_by_pkg.insert(idx, specs);
        }
    }

    let mut nodes: Vec<TextNode> = Vec::new();
    let mut drv_by_idx: BTreeMap<usize, String> = BTreeMap::new();
    let mut final_aterm = Vec::new();

    for &idx in &order {
        let p = &lock_pkgs[idx];
        let is_target = idx == target_idx;
        let drv_name = format!("{}-{}", p.name, p.version);

        let mut input_drvs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut input_srcs: BTreeSet<String> = BTreeSet::new();
        input_srcs.insert(worker.to_owned());
        input_srcs.insert(rustc_slim.to_owned());

        let mut env = BTreeMap::new();
        env.insert("__contentAddressed".into(), "1".into());
        env.insert("builder".into(), builder.clone());
        env.insert("name".into(), drv_name.clone());
        env.insert("out".into(), out_placeholder.clone());
        env.insert("outputHashAlgo".into(), "sha256".into());
        env.insert("outputHashMode".into(), "recursive".into());
        env.insert("system".into(), system.to_owned());
        env.insert("RUSTC".into(), format!("{rustc_slim}/bin/rustc"));
        env.insert("CRATE_NAME".into(), p.name.clone());
        env.insert("CRATE_VERSION".into(), p.version.clone());

        if let Some(checksum) = &p.checksum {
            // External crates.io package: fixed-output fetchurl + direct rustc lib build.
            let archive_name = format!("crate-{}-{}.tar.gz", p.name, p.version);
            let url = format!(
                "https://crates.io/api/v1/crates/{}/{}/download",
                p.name, p.version
            );
            let (fetch_drv, fetch_out, fetch_aterm, fetch_refs) =
                make_fetchurl_drv(store, &archive_name, &url, checksum);
            nodes.push(TextNode {
                path: fetch_drv.clone(),
                name: format!("{archive_name}.drv"),
                content: fetch_aterm,
                refs: fetch_refs,
            });
            input_drvs.insert(fetch_drv, vec!["out".into()]);
            env.insert("CRATE_ARCHIVE".into(), fetch_out);

            let (use_default, feats) = requested_features
                .get(&idx)
                .cloned()
                .unwrap_or_else(|| (true, BTreeSet::new()));
            env.insert(
                "DEFAULT_FEATURES".into(),
                if use_default { "1" } else { "0" }.into(),
            );
            let feat_list: Vec<String> = feats.into_iter().collect();
            env.insert("FEATURES".into(), feat_list.join(","));
        } else {
            // Workspace package.
            input_srcs.insert(src.to_owned());
            let rel = &members[&p.name];
            let full_rel = root_rel.join(rel);
            let crate_dir = if full_rel.as_os_str().is_empty() {
                src.to_owned()
            } else {
                format!("{src}/{}", full_rel.to_string_lossy())
            };
            env.insert("CRATE_DIR".into(), crate_dir);
            env.insert("SRC_ROOT".into(), src.to_owned());
            env.insert("DEFAULT_FEATURES".into(), "1".into());
            env.insert("FEATURES".into(), String::new());
        }

        // Wire dependency .rlib outputs.
        let mut extern_lines = Vec::new();
        let specs = dep_specs_by_pkg.get(&idx);
        for dep in &p.dependencies {
            let dep_idx = resolve_dep_idx(lock_pkgs, dep)?;
            let dep_pkg = &lock_pkgs[dep_idx];
            let dep_drv = &drv_by_idx[&dep_idx];
            input_drvs.insert(dep_drv.clone(), vec!["out".into()]);
            let ph = downstream_placeholder(dep_drv, "out");
            let dep_crate = dep_pkg.name.replace('-', "_");
            let aliases: Vec<String> = specs
                .map(|list| {
                    list.iter()
                        .filter(|s| s.package == dep_pkg.name)
                        .map(|s| s.alias.replace('-', "_"))
                        .collect()
                })
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| vec![dep_crate.clone()]);
            for alias in aliases {
                extern_lines.push(format!("{alias}={ph}/lib{dep_crate}.rlib"));
            }
        }
        if !extern_lines.is_empty() {
            env.insert("DRVRUST_EXTERNS".into(), extern_lines.join("\n"));
        }

        if is_target {
            // Only the linked target needs linker inputs (pure .rlib deps don't!).
            input_srcs.insert(crt_slice.to_owned());
            input_srcs.insert(binutils.to_owned());
            input_srcs.insert(glibc.to_owned());
            input_srcs.insert(gcc_lib.to_owned());
            env.insert("IS_TARGET".into(), "1".into());
            env.insert(
                "TEST_ONLY".into(),
                if filtered_test { "1" } else { "0" }.into(),
            );
            env.insert("DRVRUST_LINKER".into(), format!("{worker}/bin/drvrust-ld"));
            env.insert("DRVRUST_LD".into(), format!("{binutils}/bin/ld.bfd"));
            env.insert("DRVRUST_GLIBC".into(), glibc.to_owned());
            env.insert("DRVRUST_GCC_LIB".into(), gcc_lib.to_owned());
            env.insert("DRVRUST_CRT".into(), crt_slice.to_owned());
        } else {
            env.insert("IS_TARGET".into(), "0".into());
        }

        let drv = Derivation {
            outputs: ca_outputs(),
            input_drvs,
            input_srcs,
            platform: system.to_owned(),
            builder: builder.clone(),
            args: vec!["build-crate".into()],
            env,
        };
        let aterm = drv.to_aterm();
        if is_target {
            final_aterm = aterm;
        } else {
            let refs = drv.references();
            let drv_path = make_text_path(store, &format!("{drv_name}.drv"), &aterm, &refs);
            nodes.push(TextNode {
                path: drv_path.clone(),
                name: format!("{drv_name}.drv"),
                content: aterm,
                refs,
            });
            drv_by_idx.insert(idx, drv_path);
        }
    }

    Ok((nodes, final_aterm))
}

#[allow(clippy::too_many_arguments)]
fn plan_tier2(
    store: &str,
    system: &str,
    src: &str,
    root_rel: &Path,
    pname: &str,
    version: &str,
    selected: Option<&str>,
    filtered_test: bool,
    test_args: &str,
    native_build_inputs: &str,
    build_inputs: &str,
    lock_pkgs: &[LockPkg],
    worker: &str,
    rustc_slim: &str,
    cargo: &str,
    stdenv: &str,
) -> Result<(Vec<TextNode>, Vec<u8>), String> {
    let builder = format!("{worker}/bin/drvrust-worker");
    let out_placeholder = hash_placeholder("out");

    let mut nodes: Vec<TextNode> = Vec::new();
    let mut vendor_input_drvs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut crate_lines: Vec<String> = Vec::new();

    for p in lock_pkgs {
        if let Some(checksum) = &p.checksum {
            let archive_name = format!("crate-{}-{}.tar.gz", p.name, p.version);
            let url = format!(
                "https://crates.io/api/v1/crates/{}/{}/download",
                p.name, p.version
            );
            let (fetch_drv, fetch_out, fetch_aterm, fetch_refs) =
                make_fetchurl_drv(store, &archive_name, &url, checksum);
            nodes.push(TextNode {
                path: fetch_drv.clone(),
                name: format!("{archive_name}.drv"),
                content: fetch_aterm,
                refs: fetch_refs,
            });
            vendor_input_drvs.insert(fetch_drv, vec!["out".into()]);
            crate_lines.push(format!("{}\t{}\t{}\t{}", p.name, p.version, checksum, fetch_out));
        }
    }

    // Single parallel CA vendor derivation (replaces N separate unpack drvs + cargo-vendor-dir).
    let mut vendor_srcs = BTreeSet::new();
    vendor_srcs.insert(worker.to_owned());
    let mut vendor_env = BTreeMap::new();
    vendor_env.insert("__contentAddressed".into(), "1".into());
    vendor_env.insert("builder".into(), builder.clone());
    vendor_env.insert("name".into(), "drvrust-vendor".into());
    vendor_env.insert("out".into(), out_placeholder.clone());
    vendor_env.insert("outputHashAlgo".into(), "sha256".into());
    vendor_env.insert("outputHashMode".into(), "recursive".into());
    vendor_env.insert("system".into(), system.to_owned());
    vendor_env.insert("passAsFile".into(), "crateList".into());
    vendor_env.insert("crateList".into(), crate_lines.join("\n"));

    let vendor_drv = Derivation {
        outputs: ca_outputs(),
        input_drvs: vendor_input_drvs,
        input_srcs: vendor_srcs,
        platform: system.to_owned(),
        builder: builder.clone(),
        args: vec!["vendor-crates".into()],
        env: vendor_env,
    };
    let vendor_aterm = vendor_drv.to_aterm();
    let vendor_refs = vendor_drv.references();
    let vendor_drv_path = make_text_path(store, "drvrust-vendor.drv", &vendor_aterm, &vendor_refs);
    nodes.push(TextNode {
        path: vendor_drv_path.clone(),
        name: "drvrust-vendor.drv".into(),
        content: vendor_aterm,
        refs: vendor_refs,
    });

    // Final CA Cargo build derivation (using rustcSlim + parallel vendor dir).
    let mut build_drvs = BTreeMap::new();
    build_drvs.insert(vendor_drv_path.clone(), vec!["out".into()]);
    let mut build_srcs = BTreeSet::new();
    build_srcs.insert(worker.to_owned());
    build_srcs.insert(rustc_slim.to_owned());
    build_srcs.insert(cargo.to_owned());
    build_srcs.insert(stdenv.to_owned());
    build_srcs.insert(src.to_owned());
    for line in native_build_inputs.lines().chain(build_inputs.lines()) {
        if !line.is_empty() {
            build_srcs.insert(line.to_owned());
        }
    }

    let build_name = format!("{pname}-{version}");
    let mut build_env = BTreeMap::new();
    build_env.insert("__contentAddressed".into(), "1".into());
    build_env.insert("builder".into(), builder.clone());
    build_env.insert("name".into(), build_name);
    build_env.insert("out".into(), out_placeholder);
    build_env.insert("outputHashAlgo".into(), "sha256".into());
    build_env.insert("outputHashMode".into(), "recursive".into());
    build_env.insert("system".into(), system.to_owned());
    build_env.insert("RUSTC".into(), format!("{rustc_slim}/bin/rustc"));
    build_env.insert("CARGO".into(), format!("{cargo}/bin/cargo"));
    build_env.insert("STDENV".into(), stdenv.to_owned());
    build_env.insert("DRVRUST_SRC".into(), src.to_owned());
    build_env.insert(
        "DRVRUST_ROOT_REL".into(),
        root_rel.to_string_lossy().into_owned(),
    );
    build_env.insert(
        "DRVRUST_VENDOR".into(),
        downstream_placeholder(&vendor_drv_path, "out"),
    );
    if let Some(sel) = selected {
        build_env.insert("DRVRUST_PACKAGE".into(), sel.to_owned());
    }
    build_env.insert(
        "DRVRUST_FILTERED_TEST".into(),
        if filtered_test { "1" } else { "0" }.into(),
    );
    if !test_args.is_empty() {
        build_env.insert("DRVRUST_TEST_ARGS".into(), test_args.to_owned());
    }
    if !native_build_inputs.is_empty() {
        build_env.insert(
            "DRVRUST_NATIVE_BUILD_INPUTS".into(),
            native_build_inputs.replace('\n', " "),
        );
    }
    if !build_inputs.is_empty() {
        build_env.insert("DRVRUST_BUILD_INPUTS".into(), build_inputs.replace('\n', " "));
    }

    let build_drv = Derivation {
        outputs: ca_outputs(),
        input_drvs: build_drvs,
        input_srcs: build_srcs,
        platform: system.to_owned(),
        builder,
        args: vec!["build-cargo".into()],
        env: build_env,
    };
    Ok((nodes, build_drv.to_aterm()))
}

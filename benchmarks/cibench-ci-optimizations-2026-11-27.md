# Optimizing drvrust for CI

Across all seven CIBench CI cases, replacing `drvkit/dynamic.nix` and `nix-instantiate` with a zero-crate direct Nix daemon ATerm planner, slicing `rustc` and the GNU linker CRT/libgcc files into compact input-addressed toolchain slices, and compiling pure-Rust crates directly per crate with `rustc` reduced cold builds from 43.84 s to 6.84 s (6.41× faster), remote cache restores from 15.87 s / 699.9 MiB to 4.49 s / 275.4 MiB (3.54× faster, 60.7% less traffic), local warm checks from 1.22 s to 0.85 s (matching Nixpkgs `buildRustPackage`), remote edits from 43.91 s / 699.7 MiB to 4.90 s / 275.1 MiB (8.96× faster; 2.24× faster than `buildRustPackage` with 58.4% less cache traffic), local edits from 23.97 s to 1.29 s (18.53× faster; 1.61× faster than `buildRustPackage`), remote dependency updates from 43.73 s / 699.7 MiB to 6.12 s / 275.0 MiB (7.15× faster; 1.65× faster than `buildRustPackage` with 58.4% less traffic), and local dependency updates from 25.02 s to 1.68 s (14.91× faster; 1.28× faster than `buildRustPackage`).

CIBench builds a Rust executable (`hex-demo` depending on `hex`) and checks its printed hex output. The source edit modifies `src/main.rs` and the expected output without changing `Cargo.toml` or `Cargo.lock`. The independent dependency update upgrades `hex` from `0.4.2` to `0.4.3` in `Cargo.toml` and `Cargo.lock` without any Nix-specific lockfile hashes.

| CIBench case | drvrust `0768d63` (s) | drvrust optimized (s) | `buildRustPackage` (s) | drvrust cache transfer (before → after) | drvrust builds (before → after) | drvrust peak RAM (before → after) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| cold | 43.84 | 6.84 | 5.89 | 0.0 → 0.0 MiB | 8 → 9 | 3416 → 2828 MiB |
| remote-warm | 15.87 | 4.49 | 1.25 | 699.9 → 275.4 MiB | 0 → 0 | 3350 → 1341 MiB |
| local-warm | 1.22 | 0.85 | 0.85 | 0.0 → 0.0 MiB | 0 → 0 | 439 → 315 MiB |
| edit-remote | 43.91 | 4.90 | 11.00 | 699.7 → 275.1 MiB | 5 → 4 | 3418 → 1381 MiB |
| edit-local | 23.97 | 1.29 | 2.08 | 0.0 → 0.0 MiB | 5 → 4 | 572 → 354 MiB |
| dependency-remote | 43.73 | 6.12 | 10.10 | 699.7 → 275.0 MiB | 7 → 5 | 3403 → 1379 MiB |
| dependency-local | 25.02 | 1.68 | 2.16 | 0.0 → 0.0 MiB | 7 → 5 | 573 → 351 MiB |

Times are medians of two trials for `0768d63`, three trials for the optimized tree, and five trials for the shared Nixpkgs `buildRustPackage` comparator, run in alternating order (`before-1`, `after-1` ×2, `before-2`, `after-2`) with no competing host workloads. Both builders used Rust 1.97.1. Each sample timed evaluation and realization of the package and check together. [Individual timings, build counts, cache traffic, peak memory, and realized outputs](cibench-ci-optimizations-2026-11-27.csv)

## Summary of optimizations

1. **Direct Nix daemon ATerm planner & zero-crate Rust worker (`src/nixstore.rs`, `src/plan.rs`, `src/toml.rs`, `src/sha256.rs`, `src/tar_gz.rs`, `package.nix`)**: Replaced `drvkit/dynamic.nix`, `nix-instantiate`, and `import ${nixpkgsSource}` with a zero-crate Rust worker (`drvrust-worker`) compiled directly with `rustc` in ~1.2 s. The planner parses `Cargo.lock` (v4) and `Cargo.toml` natively, registers `builtin:fetchurl` and per-crate derivations directly over the protocol-1.21 Nix daemon Unix socket (`wopQueryValidPaths`, `wopAddTextToStore`), and removes `pkgs.nix` (~44 paths), `nixpkgsSource` (202 MiB), `boost`, `icu4c`, `bash-interactive`, the redundant inner forwarder, and ~20 s of Nixpkgs evaluation from every cold, edit, and dependency job.
2. **Compact input-addressed `rustcSlim` and `crtSlice` toolchains (`nix/toolchain.nix`, `src/linker.rs`)**:
   - Sliced `pkgs.rustc.unwrapped` (1,040 MiB NAR / 344 MiB compressed) down to `bin/rustc`, `librustc_driver-*.so`, and the target standard-library `.rlib`/`.rmeta`/`.so` files (~256 MiB NAR / ~82 MiB compressed, enforced with `disallowedReferences`), dropping ~876 MiB of `rustc_private` metadata/plugins and `rustc-src`.
   - Sliced `crtbegin*.o`, `crtend*.o`, `libgcc.a`, and `libgcc_eh.a` (~3.3 MiB NAR / ~1 MiB compressed) and added a native `drvrust-ld` wrapper around `binutils`'s `ld.bfd`, removing `gcc-15.3.0` (264 MiB NAR / 98.6 MiB compressed) from pure-Rust links.
   - Kept static toolchain slices input-addressed with substitution enabled while keeping all drvrust-generated packages, compiled crates, vendor directories, planners, tests, and forwarding derivations floating content-addressed.
3. **Two-tier per-crate compilation (`src/build.rs`, `src/plan.rs`, `package.nix`)**:
   - **Tier 1 (pure-Rust per-crate `rustc`)**: Compiles each dependency crate into its own floating CA `.rlib` derivation (requiring only `rustcSlim` and `drvrust-worker`) and links only the target binary/tests using prebuilt `.rlib`s and `drvrust-ld`. Source edits reuse every compiled dependency `.rlib` from cache, and dependency updates rebuild only the bumped crate and its dependents without invoking `cargo`, `auditable-cargo`, or `stdenv`.
   - **Tier 2 (complex Cargo projects such as `hexyl` or packages with `buildInputs`/`nativeBuildInputs`/`testArgs`)**: Still uses the direct Nix daemon planner and `rustcSlim`, unpacks all `.crate` tarballs in parallel inside a single CA `drvrust-vendor` derivation via `libz` (eliminating N separate `stdenv` unpack sandboxes), and invokes `cargo` without `auditable-cargo`.

## Correctness checks

- Realized package and check outputs were deterministic across all three optimized trials and matched between `cold`, `remote-warm`, and `local-warm`, between `edit-remote` and `edit-local`, and between `dependency-remote` and `dependency-local`.
- `nix flake check -L --option allow-import-from-derivation false` passed all four checks:
  - `checks.x86_64-linux.worker-tests` (SHA-256 NIST vectors, TOML parser, feature closure expansion, and exact `<nix/fetchurl.nix>` ATerm/store-path parity for `crate-hex-0.4.3.tar.gz`)
  - `checks.x86_64-linux.workspace` (multi-member workspace, shared library crate, unit tests, shared package/check planner, CA byte-identity across a comment-only `Cargo.toml` edit, and behavior change on a `src/main.rs` edit)
  - `checks.x86_64-linux.benchmark` (`hexyl` 0.17.0 with 68 locked crates, proc-macros, build scripts, LTO, and all 56 upstream tests passing under `rustcSlim`)
  - `checks.x86_64-linux.cli`
- `nix flake show --all-systems` (`x86_64-linux`, `aarch64-linux`) passed.
- CLI end-to-end checks passed (`drvrust build`, `drvrust test`, `drvrust test -- message`, `drvrust run`, non-zero exit status on a real test failure, and CA output reference verification showing only `glibc` and `gcc-lib`).

## Reproduction

The runner used x86_64 Linux, an Intel Core i7-1280P, Nix 2.35.3, four logical CPUs (16–19), four jobs, four cores per builder, 8 GiB RAM, and zero swap. Sandboxing was enabled and IFD disabled. The simulated project cache used 5 ms latency and 100 MiB/s aggregate bandwidth. Both builders used the same local mirror of signed NixOS packages populated from `drvkit/cibench/results/bootstrap/rust.txt`. Cold jobs started with an empty local store and project cache. Subsequent cases independently used the original job's local store or its published project cache. The recorded runner was `/nix/store/vpysz8ih9lhm154cjaamrncwjn08njp6-cibench-0.1.0`.

```sh
work=$(mktemp -d)
stock="file://$work/stock"
nix copy --from https://cache.nixos.org --to "$stock?compression=zstd&compression-level=1" \
  --stdin < ../drvkit/cibench/results/bootstrap/rust.txt
runner="$(nix build --impure --no-link --print-out-paths -f ../drvkit/cibench)/bin/cibench"
mkdir "$work/before"
git archive 0768d63 | tar -x -C "$work/before"
run() {
  taskset -c 16-19 "$runner" rust --builder "$1" --out "$work/$2" --trials "$3" \
    --cpus 4 --memory-mib 8192 --latency-ms 5 --bandwidth-mib-s 100 \
    --upstream "$stock?priority=40"
}
run "$work/before" before-1 1
run "$PWD" after-1 2
run "$work/before" before-2 1
run "$PWD" after-2 1
```

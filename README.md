# drvrust

Build Rust binaries from a version 4 `Cargo.lock`. Nix fetches each crates.io archive by its lockfile checksum, then Cargo builds and tests the project offline.

```sh
nix run github:drvlang/drvrust -- build -C ./app
./result/bin/my-app
```

For one package in a workspace, add `--options '{"package":"my-app"}'`. The flake exposes `drvrust.lib.mkFlake { src = ./app; }` and `lib.mkPackage { system, src, ... }`. Nix callers can supply `nativeBuildInputs` and `buildInputs` for native dependencies. The builder accepts crates.io and local workspace packages.

## Benchmark

The pinned hexyl 0.17.0 build runs its 56 upstream tests. The Rust benchmark runner then checks the installed executable's hex dump against known bytes. [Benchmark definition](benchmarks/default.nix) · [hexyl source](https://github.com/sharkdp/hexyl/tree/8eb6d4771ce1ec7af65d06bd335457783b77d557)

```sh
nix run github:drvlang/drvrust#bench -- --runs 5
```

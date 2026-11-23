# drvrust

Build and test Rust programs directly from a version 4 `Cargo.lock`. A dynamic Nix derivation plans the per-crate downloads from the checksums already in Cargo's lockfile. Cargo builds and tests the selected package offline. Workspace members and crates.io packages are supported.

```sh
nix run github:drvlang/drvrust -- build -C ./app
nix run github:drvlang/drvrust -- test -C ./app
nix run github:drvlang/drvrust -- run -C ./app -- --help
```

Enable `ca-derivations`, `dynamic-derivations` and `recursive-nix` in Nix and the build daemon. The builder needs the `recursive-nix` system feature. For a package that installs multiple binaries, choose one with `--options '{"program":"my-tool"}'`. Nix callers can supply native dependencies through `buildInputs` and `nativeBuildInputs`. The flake supports x86-64 and AArch64 Linux.

## Flakes and workspaces

Use `drvrust.lib.mkFlake { src = ./.; }` for one project. For a Cargo workspace, give each command the directory containing its member manifest. Cargo uses the root lockfile and can resolve shared local crates.

```nix
outputs = { drvrust, ... }: drvrust.lib.mkMonorepoFlake {
  src = ./.;
  commands = { api = "apps/api"; worker = "apps/worker"; };
};
```

Use `nix build .#api`, `nix run .#worker` and `nix flake check`. For the same names on the CLI, put `{"commands":{"api":"apps/api","worker":"apps/worker"}}` in `drvlang.json` and run `drvrust test api`. The `package` option also accepts a Cargo package name. [Working workspace](tests/workspace)

## Benchmark

The pinned hexyl 0.17.0 build runs 56 upstream tests. The Rust benchmark runner then checks the installed executable's hex dump against known bytes. [Benchmark definition](benchmarks/default.nix) · [hexyl source](https://github.com/sharkdp/hexyl/tree/8eb6d4771ce1ec7af65d06bd335457783b77d557)

```sh
nix run github:drvlang/drvrust#bench -- --runs 5
```

pkgs: { src, package ? null, nativeBuildInputs ? [], buildInputs ? [] }:
let
  lockPath = src + "/Cargo.lock";
  lock = builtins.fromTOML (builtins.readFile lockPath);
  manifest = builtins.fromTOML (builtins.readFile (src + "/Cargo.toml"));
  project = manifest.package or {};
  version = project.version or null;
  cargoFlags = pkgs.lib.optionals (package != null) [ "--package" package ];
in
assert pkgs.lib.assertMsg (lock.version == 4) "drvrust requires Cargo.lock version 4";
assert pkgs.lib.assertMsg (builtins.all (p: !(p ? source) ||
  (p.source == "registry+https://github.com/rust-lang/crates.io-index" && p ? checksum)) lock.package)
  "drvrust supports checksum-locked crates.io and workspace packages";
pkgs.rustPlatform.buildRustPackage {
  pname = if package != null then package else project.name or "cargo-workspace";
  version = if builtins.isString version then version else manifest.workspace.package.version or "workspace";
  inherit src nativeBuildInputs buildInputs;
  cargoLock.lockFile = lockPath;
  cargoBuildFlags = cargoFlags;
  cargoTestFlags = cargoFlags;
}

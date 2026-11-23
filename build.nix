pkgs: { src, memberPath, package, nativeBuildInputs, buildInputs, test, testArgs }:
let
  root = if builtins.pathExists (src + "/Cargo.lock") then src else src + "/${memberPath}";
  lockFileContents = builtins.readFile (root + "/Cargo.lock");
  lock = builtins.fromTOML lockFileContents;
  manifest = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  member = builtins.fromTOML (builtins.readFile (src + "/${memberPath}/Cargo.toml"));
  selected = if package != null then package else if memberPath == "." then null else member.package.name;
  project = member.package or {};
  version = project.version or null;
  cargoFlags = [ "--locked" ] ++ pkgs.lib.optionals (selected != null) [ "--package" selected ];
  # importCargoLock uses runCommand for both unpacked crates and the vendor
  # directory. Downloads keep the SHA-256 checksums supplied by Cargo.
  importCargoLock = pkgs.rustPlatform.importCargoLock.override {
    runCommand = name: attrs: pkgs.runCommand name (attrs // { __contentAddressed = true; });
  };
in
assert pkgs.lib.assertMsg (lock.version == 4) "drvrust requires Cargo.lock version 4";
assert pkgs.lib.assertMsg (builtins.all (p: !(p ? source) ||
  (p.source == "registry+https://github.com/rust-lang/crates.io-index" && p ? checksum)) lock.package)
  "drvrust supports checksum-locked crates.io and workspace packages";
pkgs.rustPlatform.buildRustPackage ({
  __contentAddressed = true;
  pname = if selected != null then selected else project.name or "cargo-workspace";
  version = if builtins.isString version then version else manifest.workspace.package.version or "workspace";
  src = root;
  inherit nativeBuildInputs buildInputs;
  cargoDeps = importCargoLock { inherit lockFileContents; };
  cargoBuildFlags = cargoFlags;
  cargoTestFlags = cargoFlags;
  preCheck = pkgs.lib.optionalString (testArgs != []) ''checkFlagsArray=(${pkgs.lib.escapeShellArgs testArgs})'';
} // pkgs.lib.optionalAttrs test {
  dontBuild = true;
  installPhase = ''mkdir -p "$out"'';
})

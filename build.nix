pkgs: { src, memberPath, package, nativeBuildInputs, buildInputs, test, testArgs }:
let
  root = if builtins.pathExists (src + "/Cargo.lock") then src else src + "/${memberPath}";
  lockPath = root + "/Cargo.lock";
  lock = builtins.fromTOML (builtins.readFile lockPath);
  manifest = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
  member = builtins.fromTOML (builtins.readFile (src + "/${memberPath}/Cargo.toml"));
  selected = if package != null then package else if memberPath == "." then null else member.package.name;
  project = member.package or {};
  version = project.version or null;
  cargoFlags = [ "--locked" ] ++ pkgs.lib.optionals (selected != null) [ "--package" selected ];
in
assert pkgs.lib.assertMsg (lock.version == 4) "drvrust requires Cargo.lock version 4";
assert pkgs.lib.assertMsg (builtins.all (p: !(p ? source) ||
  (p.source == "registry+https://github.com/rust-lang/crates.io-index" && p ? checksum)) lock.package)
  "drvrust supports checksum-locked crates.io and workspace packages";
pkgs.rustPlatform.buildRustPackage ({
  pname = if selected != null then selected else project.name or "cargo-workspace";
  version = if builtins.isString version then version else manifest.workspace.package.version or "workspace";
  src = root;
  inherit nativeBuildInputs buildInputs;
  cargoLock.lockFile = lockPath;
  cargoBuildFlags = cargoFlags;
  cargoTestFlags = cargoFlags;
  preCheck = pkgs.lib.optionalString (testArgs != []) ''checkFlagsArray=(${pkgs.lib.escapeShellArgs testArgs})'';
} // pkgs.lib.optionalAttrs test {
  dontBuild = true;
  installPhase = ''mkdir -p "$out"'';
})

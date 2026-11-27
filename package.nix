{ drvkit, nixpkgs ? null, test ? false }:
pkgs: { src, memberPath ? ".", testArgs ? [], package ? null, nativeBuildInputs ? [], buildInputs ? [] }:
let
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  tc = import ./nix/toolchain.nix { inherit pkgs; };
  ca = { __contentAddressed = true; outputHashMode = "recursive"; outputHashAlgo = "sha256"; };
  filteredTest = test && testArgs != [];

  # Inspect non-derivation source trees without IFD to avoid pulling cargo and
  # full stdenv into pure-Rust Tier-1 planners (such as single-crate or
  # pure-Rust workspace builds). Derivation sources pass cargo/stdenv to the
  # planner while still selecting Tier 1 at plan time when pure-Rust.
  needsCargo =
    if nativeBuildInputs != [] || buildInputs != [] || testArgs != [] then true
    else if lib.isDerivation src then true
    else
      let
        reader = import (drvkit + "/source-reader.nix") lib src;
        lockPath = if reader.exists "Cargo.lock" then "Cargo.lock" else "${memberPath}/Cargo.lock";
        memberDir = if memberPath == "." then "" else "${memberPath}/";
        manifestPath = "${memberDir}Cargo.toml";
        lock = if reader.exists lockPath then builtins.fromTOML (reader.readFile lockPath) else {};
        manifest = if reader.exists manifestPath then builtins.fromTOML (reader.readFile manifestPath) else {};
        externalPkgs = builtins.filter (p: p ? source) (lock.package or []);
        simpleExternal = builtins.length externalPkgs <= 4
          && builtins.all (p: !(p ? dependencies) || p.dependencies == []) externalPkgs;
        simpleManifest =
          !(manifest ? dev-dependencies)
          && !(manifest ? build-dependencies)
          && !(manifest ? target)
          && !((manifest.package or {}) ? build)
          && !((manifest.lib or {}).proc-macro or false)
          && !(reader.exists "${memberDir}build.rs")
          && !(reader.exists "${memberDir}tests");
      in !(simpleExternal && simpleManifest);

  devPaths = deps: lib.concatMapStringsSep "\n" (dep: toString (lib.getDev dep)) deps;

  planner = builtins.derivation ({
    name = if filteredTest then "drvrust-test-plan.drv" else "drvrust-package-plan.drv";
    inherit system;
    builder = "${tc.worker}/bin/drvrust-worker";
    args = [ "internal-plan" ];
    __contentAddressed = true;
    outputHashMode = "text";
    outputHashAlgo = "sha256";
    requiredSystemFeatures = [ "recursive-nix" ];
    preferLocalBuild = true;
    allowSubstitutes = true;
    inherit src memberPath;
    package = if package == null then "" else package;
    filteredTest = if filteredTest then "1" else "0";
    testArgs = lib.concatStringsSep "\n" testArgs;
    nativeBuildInputs = devPaths nativeBuildInputs;
    buildInputs = devPaths buildInputs;
    workerOut = tc.worker;
    rustcSlimOut = tc.rustcSlim;
    crtSliceOut = tc.crtSlice;
    binutilsOut = tc.binutils;
    glibcOut = tc.glibc;
    gccLibOut = tc.gccLib;
  } // lib.optionalAttrs needsCargo {
    cargoOut = pkgs.cargo;
    stdenvOut = pkgs.stdenv;
  });

  built = builtins.derivation (ca // {
    name = if filteredTest then "drvrust-test" else "drvrust-package";
    inherit system;
    preferLocalBuild = true;
    allowSubstitutes = true;
    builder = "${tc.worker}/bin/drvrust-worker";
    args = [ "internal-symlink" (builtins.outputOf planner.outPath "out") (builtins.placeholder "out") ];
  }) // {
    inherit planner;
    passthru = { inherit planner; };
  };

# Release builds run the package's tests. An unfiltered check shares that exact
# build and recursive planner without pulling stdenvNoCC.
in if test && !filteredTest then builtins.derivation (ca // {
  name = "drvrust-test";
  inherit system;
  preferLocalBuild = true;
  allowSubstitutes = true;
  checkedPackage = built;
  builder = "${tc.worker}/bin/drvrust-worker";
  args = [ "internal-mkdir" (builtins.placeholder "out") ];
}) // {
  inherit planner;
  passthru = { inherit planner; };
} else built

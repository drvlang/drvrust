{ drvkit, nixpkgs ? null, test ? false }:
pkgs: { src, memberPath ? ".", testArgs ? [], package ? null, nativeBuildInputs ? [], buildInputs ? [] }:
let
  inherit (pkgs) lib;
  nixpkgsSource = import (drvkit + "/nixpkgs-source.nix") pkgs nixpkgs;
  dynamic = import (drvkit + "/dynamic.nix") { inherit pkgs; };
  # Reattach dependency context inside the recursive evaluator. Select the
  # development output as stdenv would, including for split-output libraries.
  inputs = deps: lib.concatMapStringsSep " "
    (dep: "(builtins.storePath ${lib.strings.escapeNixString (toString (lib.getDev dep))})") deps;
  filteredTest = test && testArgs != [];
  built = dynamic {
    name = if filteredTest then "drvrust-test" else "drvrust-package";
    expression = ''
      let pkgs = import ${nixpkgsSource} { system = "${pkgs.stdenv.hostPlatform.system}"; };
      in import ${./build.nix} pkgs {
        src = builtins.storePath "${src}";
        memberPath = ${lib.strings.escapeNixString memberPath};
        package = ${if package == null then "null" else lib.strings.escapeNixString package};
        test = ${lib.boolToString filteredTest};
        testArgs = builtins.fromJSON ${lib.strings.escapeNixString (builtins.toJSON testArgs)};
        nativeBuildInputs = [ ${inputs nativeBuildInputs} ];
        buildInputs = [ ${inputs buildInputs} ];
      }
    '';
  };
# buildRustPackage runs the full suite before installation. An unfiltered
# check can share that exact build, including its recursive planner.
in if test && !filteredTest then pkgs.runCommand "drvrust-test" {
  __contentAddressed = true;
  checkedPackage = built;
  passthru = { inherit (built) planner; };
} ''mkdir "$out"'' else built

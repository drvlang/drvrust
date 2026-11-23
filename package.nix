{ drvkit, test ? false }:
pkgs: { src, memberPath ? ".", testArgs ? [], package ? null, nativeBuildInputs ? [], buildInputs ? [] }:
let
  inherit (pkgs) lib;
  dynamic = import (drvkit + "/dynamic.nix") { inherit pkgs; };
  # Reattach dependency context inside the recursive evaluator. Select the
  # development output as stdenv would, including for split-output libraries.
  inputs = deps: lib.concatMapStringsSep " "
    (dep: "(builtins.storePath ${lib.strings.escapeNixString (toString (lib.getDev dep))})") deps;
in dynamic {
  name = if test then "drvrust-test" else "drvrust-package";
  expression = ''
    let pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; };
    in import ${./build.nix} pkgs {
      src = builtins.storePath "${src}";
      memberPath = ${lib.strings.escapeNixString memberPath};
      package = ${if package == null then "null" else lib.strings.escapeNixString package};
      test = ${lib.boolToString test};
      testArgs = builtins.fromJSON ${lib.strings.escapeNixString (builtins.toJSON testArgs)};
      nativeBuildInputs = [ ${inputs nativeBuildInputs} ];
      buildInputs = [ ${inputs buildInputs} ];
    }
  '';
}

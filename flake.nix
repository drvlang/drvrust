{
  description = "Build Cargo-locked Rust programs with Nix";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6b5e5b7a6631f065bf6908986990b37d845f847f";
  inputs.drvkit = { url = "github:drvlang/drvkit"; flake = false; };

  outputs = { self, nixpkgs, drvkit, ... }@inputs: import drvkit {
    inherit self nixpkgs;
    toolSource = {
      root = ./.;
      files = [ ./package.nix ./nix/toolchain.nix ./src ];
      benchmarkFiles = [ ./benchmarks/default.nix ./benchmarks/source.nix ];
    };
    name = "drvrust";
    extraChecks = system: let
      pkgs = nixpkgs.legacyPackages.${system};
      args = { inherit pkgs self system; };
      tc = import ./nix/toolchain.nix { inherit pkgs; };
    in {
      worker-tests = tc.workerTests;
      benchmark = import ./benchmarks args;
      workspace = import ./tests args;
    };
    mkPackage = import ./package.nix { inherit drvkit nixpkgs; };
    mkTest = import ./package.nix { inherit drvkit nixpkgs; test = true; };
    selectTarget = { src, target, ... }: { inherit src; memberPath = target; };
  };
}

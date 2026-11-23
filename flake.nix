{
  description = "Build Cargo-locked Rust programs with Nix";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6b5e5b7a6631f065bf6908986990b37d845f847f";
  inputs.drvkit = { url = "github:drvlang/drvkit"; flake = false; };

  outputs = { self, nixpkgs, drvkit, ... }@inputs: import drvkit {
    inherit self nixpkgs;
    name = "drvrust";
    extraChecks = system: let args = {
      pkgs = nixpkgs.legacyPackages.${system}; inherit self system;
    }; in {
      benchmark = import ./benchmarks args;
      workspace = import ./tests args;
    };
    mkPackage = import ./package.nix { inherit drvkit; };
    mkTest = import ./package.nix { inherit drvkit; test = true; };
    selectTarget = { src, target, ... }: { inherit src; memberPath = target; };
  };
}

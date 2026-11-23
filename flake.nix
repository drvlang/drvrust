{
  description = "Build Cargo-locked Rust programs with Nix";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/6b5e5b7a6631f065bf6908986990b37d845f847f";
  inputs.drvkit = { url = "github:drvlang/drvkit"; flake = false; };

  outputs = { self, nixpkgs, drvkit, ... }@inputs: import drvkit {
    inherit self nixpkgs;
    name = "drvrust";
    extraChecks = system: { benchmark = import ./benchmarks {
      pkgs = nixpkgs.legacyPackages.${system}; inherit self system;
    }; };
    mkPackage = import ./package.nix;
  };
}

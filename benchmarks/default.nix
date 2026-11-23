{ self, pkgs, system }:
let hexyl = self.lib.mkPackage { inherit system; src = (import ./source.nix).outPath; };
in pkgs.runCommand "drvrust-hexyl-output" { __contentAddressed = true; } ''
  printf 'drvlang' | ${hexyl}/bin/hexyl --color=never > "$out"
  grep -F '64 72 76 6c 61 6e 67' "$out"
''

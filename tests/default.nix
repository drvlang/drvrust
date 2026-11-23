{ self, system, pkgs }:
let
  root = ./workspace;
  commands = (builtins.fromJSON (builtins.readFile ./workspace/drvlang.json)).commands;
  common = [
    ./workspace/Cargo.toml
    ./workspace/Cargo.lock
    ./workspace/pkg/shared/Cargo.toml
    ./workspace/pkg/shared/src
  ];
  srcForTarget = { target, ... }: let member = root + "/${target}";
  in self.lib.projectSource {
    inherit root;
    files = common ++ [ (member + "/Cargo.toml") (member + "/src") ];
  };
  workspace = self.lib.mkMonorepoFlake { inherit commands srcForTarget; };
  apps = workspace.apps.${system};
in pkgs.runCommand "check-workspace" {
  memberTests = builtins.attrValues workspace.checks.${system};
} ''
  test "$(${apps.api.program})" = 'api drvlang'
  test "$(${apps.worker.program})" = 'worker drvlang'
  touch "$out"
''

{ self, system, pkgs }:
let
  workspace = self.lib.mkMonorepoFlake { src = ./workspace; };
  apps = workspace.apps.${system};
in pkgs.runCommand "check-workspace" {
  memberTests = builtins.attrValues workspace.checks.${system};
} ''
  test "$(${apps.api.program})" = 'api drvlang'
  test "$(${apps.worker.program})" = 'worker drvlang'
  touch "$out"
''

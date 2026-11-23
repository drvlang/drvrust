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
  api = workspace.packages.${system}.api;
  editedApi = edit: self.lib.mkPackage {
    inherit system commands;
    target = "api";
    src = pkgs.runCommand "project-source" { __contentAddressed = true; } ''
      cp -r ${srcForTarget { target = commands.api; }} "$out"
      chmod -R u+w "$out"
      cd "$out"
      ${edit}
    '';
  };
  manifestEdit = editedApi ''echo "# edited manifest" >> apps/api/Cargo.toml'';
  programEdit = editedApi ''sed -i 's/api {}/edited {}/' apps/api/src/main.rs'';
in
assert api.planner.drvPath == workspace.checks.${system}.api.planner.drvPath;
pkgs.runCommand "check-workspace" {
  __contentAddressed = true;
  memberTests = builtins.attrValues workspace.checks.${system};
} ''
  test "$(${apps.api.program})" = 'api drvlang'
  test "$(${apps.worker.program})" = 'worker drvlang'
  test "${api}" = "${manifestEdit}"
  test "${api}" != "${programEdit}"
  test "$(${programEdit}/bin/api)" = 'edited drvlang'
  touch "$out"
''

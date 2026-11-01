# Evaluates the Home Manager module in a real Home Manager configuration and
# checks the files it generates: settings, the login service, and no secrets.
{
  lib,
  runCommand,
  jq,
  home-manager,
  module,
  pkgs,
}:
let
  evaluate =
    pigeon:
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        module
        {
          home.username = "check";
          home.homeDirectory = "/home/check";
          home.stateVersion = "25.11";
          programs.pigeon = pigeon;
        }
      ];
    }).config;

  configured = evaluate {
    enable = true;
    settings = {
      closeToBackground = false;
      notifications = "count";
    };
    autostart.enable = true;
  };
  appOwned = evaluate {
    enable = true;
    settings = null;
  };

  settings = configured.xdg.configFile."pigeon/settings.json".source;
  unit = configured.xdg.configFile."systemd/user/pigeon.service".source;
  package = configured.programs.pigeon.package;
in
assert lib.assertMsg (lib.elem package configured.home.packages) "the package is installed";
assert lib.assertMsg (
  !(appOwned.xdg.configFile ? "pigeon/settings.json")
) "settings = null leaves the file to the app";
assert lib.assertMsg (
  !(appOwned.xdg.configFile ? "systemd/user/pigeon.service")
) "autostart is off by default";
runCommand "pigeon-home-manager-check" { nativeBuildInputs = [ jq ]; } ''
  set -eu
  echo "settings.json:"; cat ${settings}
  jq -e '. == {"closeToBackground": false, "notifications": "count", "senderImages": false, "tray": false}' ${settings}

  echo "pigeon.service:"; cat ${unit}
  grep -qx 'ExecStart=${lib.getExe package} --background' ${unit}
  grep -qx 'Restart=on-failure' ${unit}
  grep -qx 'WantedBy=graphical-session.target' ${unit}

  if grep -Eiq 'password|secret|token' ${settings} ${unit}; then
    echo "generated files must not carry credentials" >&2
    exit 1
  fi
  touch $out
''

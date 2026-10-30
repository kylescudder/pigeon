# Home Manager module: non-secret desktop settings and starting at login.
# Account passwords and the database key live in the Secret Service keyring;
# nothing here may hold them, because everything written becomes a world-readable
# store path.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.pigeon;
  json = pkgs.formats.json { };
in
{
  options.programs.pigeon = {
    enable = lib.mkEnableOption "Pigeon, a local-first email client";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      defaultText = lib.literalExpression "pigeon.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "The Pigeon package to install and start.";
    };

    settings = lib.mkOption {
      default = { };
      description = ''
        Desktop settings written to {file}`$XDG_CONFIG_HOME/pigeon/settings.json`.
        The app shows them as managed and read-only. Setting this to `null`
        leaves the file to the app, so they can be changed in Settings.
      '';
      type = lib.types.nullOr (
        lib.types.submodule {
          options = {
            closeToBackground = lib.mkOption {
              type = lib.types.bool;
              default = true;
              description = "Keep running and syncing when the window closes. Quit Pigeon always exits.";
            };
            notifications = lib.mkOption {
              type = lib.types.enum [
                "senderSubject"
                "count"
                "off"
              ];
              default = "senderSubject";
              description = "What new-mail notifications show. Message bodies are never shown.";
            };
            tray = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Show a status-area icon when a tray host is running. Pigeon works fully without one.";
            };
          };
        }
      );
    };

    autostart = {
      enable = lib.mkEnableOption "starting Pigeon in the background at login, as the systemd user service {file}`pigeon.service`";

      target = lib.mkOption {
        type = lib.types.str;
        default = "graphical-session.target";
        description = ''
          The user target that starts Pigeon. Compositors started without
          systemd integration (for example Hyprland without UWSM) may never
          reach `graphical-session.target`; start the service from the
          compositor instead, after importing `WAYLAND_DISPLAY` into systemd.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    xdg.configFile."pigeon/settings.json" = lib.mkIf (cfg.settings != null) {
      source = json.generate "pigeon-settings.json" cfg.settings;
    };

    systemd.user.services.pigeon = lib.mkIf cfg.autostart.enable {
      Unit = {
        Description = "Pigeon email client";
        PartOf = [ cfg.autostart.target ];
        After = [ cfg.autostart.target ];
      };
      Service = {
        ExecStart = "${lib.getExe cfg.package} --background";
        # Quit Pigeon exits cleanly and stays quit; only crashes restart.
        Restart = "on-failure";
        RestartSec = 5;
      };
      Install.WantedBy = [ cfg.autostart.target ];
    };
  };
}

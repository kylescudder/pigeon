# Upstream-shaped Home Manager module: this file becomes
# modules/programs/pigeon.nix in nix-community/home-manager, apart from
# `meta.maintainers` (see docs/home-manager-upstream.md). The flake wraps it in
# ../home-manager.nix.
#
# Account passwords and the database key live in the Secret Service keyring;
# nothing here may hold them, because everything written becomes a world-readable
# store path.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.pigeon;
  json = pkgs.formats.json { };

  emailAccounts = config.accounts.email.accounts;
  accounts = lib.filterAttrs (_: account: account.enable && account.pigeon.enable) emailAccounts;

  security = tls: if tls.useStartTls then "start_tls" else "tls";
  defaultPort =
    tls: implicit: starttls:
    if tls.useStartTls then starttls else implicit;

  # Pigeon trusts the system roots; only a per-account override is passed on.
  customCertificates =
    account:
    let
      files = lib.filter (file: file != null && file != config.accounts.email.certificatesFile) (
        map (server: if server == null then null else server.tls.certificatesFile) [
          account.imap
          account.smtp
        ]
      );
    in
    if files == [ ] then null else toString (lib.head files);

  accountJson = account: {
    inherit (account) address primary;
    inherit (account) realName userName;
    imap = {
      inherit (account.imap) host;
      port =
        if account.imap.port != null then account.imap.port else defaultPort account.imap.tls 993 143;
      security = security account.imap.tls;
    };
    smtp =
      if account.smtp == null then
        null
      else
        {
          inherit (account.smtp) host;
          port =
            if account.smtp.port != null then account.smtp.port else defaultPort account.smtp.tls 465 587;
          security = security account.smtp.tls;
        };
    caCertificate = customCertificates account;
    signature = if account.signature.showSignature == "append" then account.signature.text else "";
  };

  accountsFile = {
    version = 1;
    accounts = lib.mapAttrs (_: accountJson) accounts;
  };

  # Authentication Pigeon can do: a plain username and password.
  passwordAuth =
    authentication:
    lib.elem authentication [
      null
      "plain"
      "login"
    ];
in
{
  meta.maintainers = [ ];

  options.programs.pigeon = {
    enable = lib.mkEnableOption "Pigeon, a local-first email client";

    package = lib.mkPackageOption pkgs "pigeon-mail" { };

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
            senderImages = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Look up website icons for senders. This tells DuckDuckGo which domains email you; off shows initials.";
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

  options.accounts.email.accounts = lib.mkOption {
    type = lib.types.attrsOf (
      lib.types.submodule {
        options.pigeon.enable = lib.mkEnableOption ''
          Pigeon for this email account. The server settings are written to
          {file}`$XDG_CONFIG_HOME/pigeon/accounts.json` and shown in the app as
          managed. The password is never written: enter it once in Pigeon and
          it stays in the desktop keyring, so `passwordCommand` is ignored
        '';
      }
    );
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      (lib.hm.assertions.assertPlatform "programs.pigeon" pkgs lib.platforms.linux)
    ]
    ++ lib.concatLists (
      lib.mapAttrsToList (name: account: [
        {
          assertion = builtins.match "[A-Za-z0-9_-]{1,64}" name != null;
          message = "Pigeon needs accounts.email.accounts.${name} to be named with up to 64 letters, digits, - or _.";
        }
        {
          assertion = account.imap != null && account.imap.tls.enable;
          message = "Pigeon needs an IMAP server with TLS or STARTTLS for accounts.email.accounts.${name}.";
        }
        {
          assertion = account.smtp == null || account.smtp.tls.enable;
          message = "Pigeon needs SMTP with TLS or STARTTLS for accounts.email.accounts.${name}.";
        }
        {
          assertion =
            account.imap == null
            || (
              passwordAuth account.imap.authentication
              && (account.smtp == null || passwordAuth account.smtp.authentication)
            );
          message = "Pigeon supports password sign-in only (not OAuth) for accounts.email.accounts.${name}.";
        }
      ]) accounts
    );

    home.packages = [ cfg.package ];

    xdg.configFile."pigeon/settings.json" = lib.mkIf (cfg.settings != null) {
      source = json.generate "pigeon-settings.json" cfg.settings;
    };

    xdg.configFile."pigeon/accounts.json" = lib.mkIf (accounts != { }) {
      source = json.generate "pigeon-accounts.json" accountsFile;
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

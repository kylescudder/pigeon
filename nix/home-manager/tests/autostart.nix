{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage {
      name = "pigeon";
      outPath = "@pigeon@";
    };
    autostart.enable = true;
  };

  nmt.script = ''
    assertFileContent home-files/.config/systemd/user/pigeon.service ${builtins.toFile "expected.service" ''
      [Install]
      WantedBy=graphical-session.target

      [Service]
      ExecStart=@pigeon@/bin/pigeon --background
      Restart=on-failure
      RestartSec=5

      [Unit]
      After=graphical-session.target
      Description=Pigeon email client
      PartOf=graphical-session.target
    ''}
  '';
}

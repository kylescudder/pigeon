{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
  };

  nmt.script = ''
    assertFileContent home-files/.config/pigeon/settings.json ${builtins.toFile "expected.json" ''
      {
        "closeToBackground": true,
        "notifications": "senderSubject",
        "tray": false
      }
    ''}
    assertPathNotExists home-files/.config/systemd/user/pigeon.service
  '';
}

{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
    settings = {
      closeToBackground = false;
      notifications = "count";
      tray = true;
    };
  };

  nmt.script = ''
    assertFileContent home-files/.config/pigeon/settings.json ${builtins.toFile "expected.json" ''
      {
        "closeToBackground": false,
        "notifications": "count",
        "tray": true
      }
    ''}
  '';
}

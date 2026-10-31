{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
    settings = null;
  };

  nmt.script = ''
    assertPathNotExists home-files/.config/pigeon/settings.json
  '';
}

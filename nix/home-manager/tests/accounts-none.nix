{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
  };

  accounts.email.accounts.other = {
    primary = true;
    address = "other@example.com";
    realName = "Other";
    userName = "other";
    imap.host = "imap.other.example.com";
  };

  nmt.script = ''
    assertPathNotExists home-files/.config/pigeon/accounts.json
  '';
}

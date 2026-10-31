{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
  };

  accounts.email.accounts."my mail" = {
    primary = true;
    address = "me@example.com";
    realName = "Me";
    userName = "me";
    imap.host = "imap.example.com";
    pigeon.enable = true;
  };

  test.asserts.assertions.expected = [
    "Pigeon needs accounts.email.accounts.my mail to be named with up to 64 letters, digits, - or _."
  ];
}

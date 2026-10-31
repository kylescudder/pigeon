{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
  };

  accounts.email.accounts.work = {
    primary = true;
    address = "me@example.com";
    realName = "Me";
    userName = "me";
    imap = {
      host = "imap.example.com";
      authentication = "xoauth2";
    };
    pigeon.enable = true;
  };

  test.asserts.assertions.expected = [
    "Pigeon supports password sign-in only (not OAuth) for accounts.email.accounts.work."
  ];
}

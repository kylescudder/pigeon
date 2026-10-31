{ config, ... }:
{
  programs.pigeon = {
    enable = true;
    package = config.lib.test.mkStubPackage { name = "pigeon"; };
  };

  accounts.email.accounts = {
    personal = {
      primary = true;
      address = "kyle@example.com";
      realName = "Kyle Example";
      userName = "kyle";
      imap.host = "imap.example.com";
      smtp = {
        host = "smtp.example.com";
        tls.useStartTls = true;
      };
      signature = {
        text = "Kyle";
        showSignature = "append";
      };
      pigeon.enable = true;
    };
    # A plain account from the shared set is left out unless it opts in.
    other = {
      address = "other@example.com";
      realName = "Other";
      userName = "other";
      imap.host = "imap.other.example.com";
    };
  };

  nmt.script = ''
    assertFileContent home-files/.config/pigeon/accounts.json ${builtins.toFile "expected.json" ''
      {
        "accounts": {
          "personal": {
            "address": "kyle@example.com",
            "caCertificate": null,
            "imap": {
              "host": "imap.example.com",
              "port": 993,
              "security": "tls"
            },
            "primary": true,
            "realName": "Kyle Example",
            "signature": "Kyle",
            "smtp": {
              "host": "smtp.example.com",
              "port": 587,
              "security": "start_tls"
            },
            "userName": "kyle"
          }
        },
        "version": 1
      }
    ''}
  '';
}

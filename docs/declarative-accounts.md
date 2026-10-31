# Declarative accounts (Home Manager)

Goal: declare mail accounts in Nix, using Home Manager's standard
`accounts.email.accounts` schema (the one aerc, neomutt, Thunderbird and
himalaya share), so a new machine gets its accounts back and the user only
types passwords.

## Module side (done)

An account is shared by every program that opts in, so Pigeon adds one option
per account, as the other modules do:

```nix
programs.pigeon.enable = true;

accounts.email.accounts.personal = {
  primary = true;
  address = "me@example.com";
  realName = "Me";
  userName = "me";
  flavor = "gmail.com";        # or imap.host / smtp.host by hand
  pigeon.enable = true;        # opt this account in
};
```

Only accounts with `enable` and `pigeon.enable` are written. Presets such as
`flavor` work unchanged because Pigeon reads the resolved values.

Rules the module enforces (as assertions):

- IMAP is required, and IMAP and SMTP must use TLS or STARTTLS.
- Sign-in is a password only: `authentication` must be unset, `plain` or `login`
  (no OAuth yet).
- `passwordCommand` is ignored. Passwords are never in Nix, because anything
  written there becomes a world-readable store path; they stay in the keyring.

## The file: `$XDG_CONFIG_HOME/pigeon/accounts.json`

```json
{
  "version": 1,
  "accounts": {
    "personal": {
      "address": "me@example.com",
      "realName": "Me",
      "userName": "me",
      "primary": true,
      "imap": { "host": "imap.example.com", "port": 993, "security": "tls" },
      "smtp": { "host": "smtp.example.com", "port": 587, "security": "start_tls" },
      "caCertificate": null,
      "signature": ""
    }
  }
}
```

- The key (`personal`) is the account's stable identity. Renaming it makes a new
  account.
- Ports are resolved by the module (993/143 for IMAP, 465/587 for SMTP).
- `smtp` is `null` for receive-only accounts.
- `caCertificate` is a file path, and only set when an account overrides the
  global `accounts.email.certificatesFile`; the system roots are not passed on.
- `signature` is the text to append, empty unless `signature.showSignature` is
  `append`.
- `security` uses the same names as the app's stored accounts.

## App side (not done)

The app does not read this file yet. The intended behaviour:

1. **Stable identity.** A managed account's id is derived from its key (for
   example `managed:personal`), not random, so the keyring password and cached
   mail survive a rebuild.
2. **Reconcile on start and when the file changes.** Missing accounts are
   created without a password and show "enter your password". Changed server
   settings are updated in place.
3. **Read-only in the UI.** Managed accounts show server settings as managed
   (as settings already do), still accept a password, and cannot be removed
   in the app.
4. **Removal is not deletion.** An account that disappears from the file stops
   being managed and stays as an ordinary account, so a config mistake never
   deletes cached mail.
5. **Backup and restore.** Restoring on another machine keeps managed accounts
   managed if the file still lists them.

The engine currently verifies the password when an account is added, so the
main change is a "created, waiting for a password" state in the engine.

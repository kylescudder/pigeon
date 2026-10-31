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

- The account name is up to 64 letters, digits, `-` or `_`.
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

## App side

On every mail refresh the app reads the file and reconciles it with its store:

1. **Stable identity.** A declared account's id is `managed-<name>`, so its
   keyring password and cached mail survive a rebuild.
2. **Created without a password.** The account appears with "enter this
   account's password to start syncing". The password is verified when entered,
   as for any account, and kept in the keyring.
3. **Updated in place.** Changed server settings, display name and signature
   apply without a restart. The "save a copy to Sent" preference is not
   declared and stays as the user set it.
4. **Read-only in the UI.** Managed accounts cannot be removed or have their
   sending settings edited in the app (the engine refuses too), and a request
   from the interface cannot mark an account managed.
5. **Removal is not deletion.** An account that disappears from the file stops
   being managed and stays as an ordinary account, so a config mistake never
   deletes cached mail.
6. **Bad files change nothing.** A file that cannot be parsed, or has an
   unknown `version`, leaves managed accounts as they are and shows a warning.
   A bad entry is skipped with a warning and the rest still apply.
7. **Backup and restore.** A restored account keeps its managed flag and is
   re-reconciled against the file on the next refresh.

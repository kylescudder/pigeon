# Pigeon compatibility patch for imap-proto 0.10.2

This is the source of the published `imap-proto` 0.10.2 crate, originating at
upstream commit `08f269400ec95b176d30cac7b23730820fe94d7e`:
https://github.com/djc/imap-proto/tree/08f269400ec95b176d30cac7b23730820fe94d7e

Original crates.io archive SHA-256:
`16a6def1d5ac8975d70b3fd101d57953fe3278ef2ee5d7816cba54b1d1dfc22f`.
`Cargo.toml` comes from its original manifest. Both upstream licence files are
included. Pigeon uses the MIT licence option; upstream authorship is retained.

The root Cargo `[patch.crates-io]` selects this copy for both `imap` and Pigeon.
Nix explicitly includes `vendor/` in its source fileset; this is not a build-time
network patch or an edit to a mutable Cargo registry cache.

Local changes:

- `body_structure.rs`: consume all trailing body-extension values, as required
  by RFC 3501 section 9, preserving the original representation for zero/one.
- `body_structure.rs`: accept an unquoted Content-ID atom specifically in that
  MIME field, accommodating a response form reported to Zoho. Other framing,
  numeric fields and MIME data remain strict. No raw-data rewriting is used.
- `parser/rfc3501.rs`: backport wildcard FLAGS acceptance from upstream PR #91,
  commit `fc31a0e`. https://github.com/djc/imap-proto/pull/91
- `macros.rs`: remove expression-position trailing semicolons, now rejected for
  local crates by current Rust. This does not change parsing behaviour.

The Zoho report is historical and does not establish the exact current response
of the user's account:
https://help.zoho.com/portal/hi/community/topic/bug-in-imap-server-protocol

Regression coverage is in Pigeon's `imap_compatibility` integration test and the
actual scripted FETCH exchange in `mail::imap::tests`. Invalid syntax is still
rejected; unreadable responses retain bounded isolation/incomplete-sync handling.
The unchanged upstream suite can be run independently with:

    cargo test --release --locked --manifest-path vendor/imap-proto/Cargo.toml

The standalone lockfile is for that upstream test suite. CI also runs it. The
root lockfile controls application builds. Nom 5 still emits legacy non-local
macro/lifetime warnings; these are not suppressed by application lint settings.

Remove this local patch when the IMAP adapter moves to an upstream version that
passes all these compatibility/security tests. Do not replace it blindly during
an automated dependency update. No TLS, EXAMINE/PEEK, message sanitisation,
attachment selection or database encryption settings are changed here.

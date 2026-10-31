# Getting Pigeon into Home Manager

Today people use Pigeon's own flake module (`homeManagerModules.default`, see
the README). `programs.pigeon` only works without extra flake inputs once it is
in Home Manager itself, and Home Manager only takes modules for programs that
are packaged in nixpkgs. So there are two upstream steps, in this order.

## State of this repository

- `nix/home-manager/module.nix` is the module in Home Manager's own style:
  `mkPackageOption pkgs "pigeon"`, a Linux platform assertion, and no flake
  references. `nix/home-manager.nix` imports it and only sets the package
  default to this flake's build.
- `nix/home-manager/tests/` holds Home Manager `nmt` tests (default settings,
  custom settings, `settings = null`, autostart). They pass against Home
  Manager master as of 2026-10-31.
- The flake's `checks.<system>.home-manager` still evaluates the wrapper in a
  real Home Manager configuration.

## 1. nixpkgs: package `pigeon`

Needs, before the nixpkgs pull request:

1. A tagged release (for example `v0.1.0`) that `fetchFromGitHub` can pin.
2. A maintainer entry for yourself in `maintainers/maintainer-list.nix`,
   as a separate first commit.
3. `pkgs/by-name/pi/pigeon/package.nix`, adapted from `nix/package.nix`:
   `fetchFromGitHub` instead of the local fileset, `cargoHash` instead of
   `cargoLock`, and the Bun `node_modules` fixed-output derivation kept (hash
   from `tools/update-node-modules-hash.sh`). Drop `passthru` checks that need
   the flake, and add `meta.maintainers`.
4. Build it on x86_64-linux with `nix-build -A pigeon` and run
   `nixpkgs-review pr` before opening the pull request.

## 2. Home Manager: `programs.pigeon`

After the package is in nixpkgs unstable:

1. Copy `nix/home-manager/module.nix` to `modules/programs/pigeon.nix`
   (modules in that directory are imported automatically) and set
   `meta.maintainers` to your nixpkgs maintainer entry.
2. Copy `nix/home-manager/tests/*.nix` to `tests/modules/programs/pigeon/`.
3. Add a news entry with `./modules/misc/news/create-news-entry.sh`.
4. Run the tests from the Home Manager checkout:

   ```sh
   nix-build --pure tests --option allow-import-from-derivation false \
     -A build.pigeon-default -A build.pigeon-settings \
     -A build.pigeon-app-owned-settings -A build.pigeon-autostart \
     -A build.pigeon-accounts -A build.pigeon-accounts-none \
     -A build.pigeon-accounts-oauth
   ```

5. Commit as `pigeon: add module` (maintainer entry in its own earlier commit
   if you are not in nixpkgs' list) and open the pull request.

Once it lands, the README can drop the flake input and Home Manager users just
set `programs.pigeon.enable = true`.

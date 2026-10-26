{
  description = "Pigeon: a modern, local-first email client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      # x86_64-linux is the supported target; aarch64-linux is built for
      # development containers only.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        pigeon = pkgs.callPackage ./nix/package.nix { };
        default = pigeon;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.pigeon}/bin/pigeon";
        };
      });

      checks = forAllSystems (
        pkgs:
        let
          pigeon = self.packages.${pkgs.stdenv.hostPlatform.system}.pigeon;
        in
        {
          package = pigeon;
          core-tests = pigeon.passthru.coreTests;
          frontend = pigeon.passthru.frontendChecks;
          # Runs the packaged binary's self-test in real WebKitGTK with a
          # throwaway keyring: SQLCipher/FTS5, keyring unlock, and viewer IPC denial.
          selftest = pkgs.callPackage ./nix/selftest.nix { inherit pigeon; };
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.pigeon ];
          packages = with pkgs; [
            bun
            cargo-tauri
            clippy
            rustfmt
            rust-analyzer
          ];
          # WebKitGTK's DMA-BUF renderer misbehaves on some GPU stacks.
          WEBKIT_DISABLE_DMABUF_RENDERER = "1";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}

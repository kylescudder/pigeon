# Flake wrapper around the upstream-shaped module: the same options, with the
# package defaulting to this flake's build until Pigeon is in nixpkgs.
{ self }:
{ lib, pkgs, ... }:
{
  imports = [ ./home-manager/module.nix ];

  config.programs.pigeon.package =
    lib.mkDefault
      self.packages.${pkgs.stdenv.hostPlatform.system}.default;
}

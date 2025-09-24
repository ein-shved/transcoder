{
  description = ''
    A simple transcoding utility
  '';

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };
        transcoder = pkgs.callPackage ./. { };
      in
      {
        packages = {
          inherit transcoder;
          default = transcoder;
        };
        devShells.default = pkgs.mkShell {
          inputsFrom = [ transcoder ];
          packages = with pkgs; [
            rust-analyzer
            rustfmt
          ];
        };
      }
    ) // {
        formatter = nixpkgs.formatter;
    };
}

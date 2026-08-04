{
  description = "served per-user service manager";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.nixpkgs-x86-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";

  outputs =
    {
      self,
      nixpkgs,
      nixpkgs-x86-darwin,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
      nixpkgsFor = system: if system == "x86_64-darwin" then nixpkgs-x86-darwin else nixpkgs;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import (nixpkgsFor system) { inherit system; };
          served = pkgs.callPackage ./nix/package.nix { servedSource = self; };
        in
        {
          inherit served;
          default = served;
        }
      );

      nixosModules = {
        served =
          {
            lib,
            pkgs,
            ...
          }:
          {
            imports = [ ./nix/module.nix ];
            services.served.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.served;
          };
        default = self.nixosModules.served;
      };

      checks = forAllSystems (
        system:
        let
          pkgs = import (nixpkgsFor system) { inherit system; };
        in
        {
          package = self.packages.${system}.served;
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          multi-user = import ./nix/tests/multi-user.nix {
            inherit pkgs;
            servedModule = self.nixosModules.served;
          };
        }
      );

      formatter = forAllSystems (system: (import (nixpkgsFor system) { inherit system; }).nixfmt);
    };
}

{
  description = "Find, resume and fork coding-agent sessions across Claude Code, Codex and more";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor = system: import nixpkgs { inherit system; };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          hopback = pkgs.callPackage ./nix/package.nix { };
        in
        {
          inherit hopback;
          default = hopback;
        }
      );

      # `nix run github:avgvi/hopback` starts `hopback`;
      # `nix run github:avgvi/hopback#hb` starts the short command.
      apps = forAllSystems (
        system:
        let
          bin = name: {
            type = "app";
            program = "${self.packages.${system}.default}/bin/${name}";
          };
        in
        {
          default = bin "hopback";
          hopback = bin "hopback";
          hb = bin "hb";
        }
      );

      checks = forAllSystems (system: {
        package = self.packages.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              rustc
              rustfmt
            ];
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}

{
  description = "Hyprland mouse bridge for DeskDock's Synergy/Barrier integration";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
          hypr-android-bridge = pkgs.rustPlatform.buildRustPackage {
            pname = "hypr-android-bridge";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            # The bridge shells out to adb to follow Android's rotation.
            postInstall = ''
              wrapProgram $out/bin/hypr-android-bridge \
                --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.android-tools ]}
            '';
            meta = {
              description = "Hyprland mouse bridge for DeskDock's Synergy/Barrier integration";
              license = pkgs.lib.licenses.mit;
              mainProgram = "hypr-android-bridge";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        in {
          default = hypr-android-bridge;
          inherit hypr-android-bridge;
        });

      overlays.default = final: _prev: {
        hypr-android-bridge = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      checks = forAllSystems (system: {
        build = self.packages.${system}.default;
      });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/hypr-android-bridge";
        };
      });

      devShells = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              clippy
              rust-analyzer
              pkg-config
              android-tools
              hyprland
            ];
          };
        });
    };
}

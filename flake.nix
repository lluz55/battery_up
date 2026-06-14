{
  description = "battery-up: measure notebook time running only on battery";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              let
                name = baseNameOf path;
              in
              !(type == "directory" && builtins.elem name [
                ".agents"
                ".codex"
                ".git"
                "result"
                "target"
              ]);
          };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "battery-up";
            version = "0.1.0";
            inherit src;
            cargoHash = "sha256-3TJSuQNjIKQo7M69LR0cOvnTKTX4W2qZimyC11Hmh/E=";
            nativeBuildInputs = with pkgs; [
              pkg-config
            ];
            buildInputs = with pkgs; [
              dbus.dev
              glib
              libglvnd
              libxkbcommon
              wayland
            ];
            runtimeDependencies = with pkgs; [
              libglvnd
              wayland
            ];
            postInstall = ''
              substituteInPlace data/applications/dev.lluz.BatteryUpApplet.desktop \
                --replace-fail 'Exec=cosmic-applet-battery-up' "Exec=$out/bin/cosmic-applet-battery-up"
              install -Dm0644 data/applications/dev.lluz.BatteryUpApplet.desktop \
                $out/share/applications/dev.lluz.BatteryUpApplet.desktop
              install -Dm0644 data/icons/scalable/apps/dev.lluz.BatteryUpApplet-symbolic.svg \
                $out/share/icons/hicolor/scalable/apps/dev.lluz.BatteryUpApplet-symbolic.svg
            '';
          };
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/battery-up";
          meta.description = "Measure notebook time running only on battery";
        };
      });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.battery-up;
        in
        {
          options.services.battery-up = {
            enable = lib.mkEnableOption "battery-up background battery-only timer";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.default;
              defaultText = "battery-up package from this flake";
              description = "battery-up package to run.";
            };

            interval = lib.mkOption {
              type = lib.types.ints.positive;
              default = 1;
              description = "Polling interval in seconds.";
            };

            stateFile = lib.mkOption {
              type = lib.types.str;
              default = "/var/lib/battery-up/state";
              description = "File where the daemon stores the accumulated time.";
            };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.battery-up = {
              description = "Track notebook time running only on battery";
              wantedBy = [ "multi-user.target" ];
              after = [ "multi-user.target" ];
              serviceConfig = {
                ExecStart = "${lib.getExe cfg.package} daemon --interval ${toString cfg.interval} --state-file ${cfg.stateFile}";
                Restart = "always";
                RestartSec = "5s";
                StateDirectory = "battery-up";
              };
            };
          };
        };

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.dbus.dev
              pkgs.glib
              pkgs.libglvnd
              pkgs.libxkbcommon
              pkgs.pkg-config
              pkgs.rustc
              pkgs.rustfmt
              pkgs.wayland
            ];

            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.libglvnd
              pkgs.wayland
            ];
          };
        });
    };
}

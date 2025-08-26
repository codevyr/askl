{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
    devenv = {
      url = "github:cachix/devenv";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, flake-utils, devenv, crane, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        # Use mkLib (works across crane versions)
        craneLib = (crane.mkLib or crane.lib.mkLib) pkgs;

        flakeInputs = { inherit nixpkgs flake-utils devenv crane self; };

        # Keep the whole repo so ../../sql/* is present during build
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          # Keep everything except VCS/target; adjust as you like
          filter = path: type:
            let p = toString path;
            in ! (pkgs.lib.hasInfix "/.git/" p || pkgs.lib.hasInfix "/target/" p);
        };
        # Common build settings for all workspace members
        common = {
          src = src;

          # >>> CHANGE: use `env` instead of `buildEnv`
          env = {
            SQLX_OFFLINE = "true";
          };

          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [
            pkgs.sqlite
            # pkgs.openssl
            # pkgs.llvmPackages_12.libclang
          ];

          cargoExtraArgs = "--locked";
        };

        # Build a specific workspace member
        mkCrate = name:
          craneLib.buildPackage (common // {
            pname = name;
            version = "0.1.0";
            cargoExtraArgs = "${common.cargoExtraArgs} -p ${name}";
          });

        members = [ "askld" "create-index" ];

        built = builtins.listToAttrs (map (n: {
          name = n;
          value = mkCrate n;
        }) members);

        # Use copyToRoot (contents is deprecated)
        mkImage = name:
          let
            rootfs = pkgs.buildEnv {
              name = "${name}-rootfs";
              paths = [
                built.${name}
                pkgs.cacert
                pkgs.tzdata
                pkgs.sqlite
              ];
              pathsToLink = [ "/bin" "/etc" "/share" "/lib" ];
            };
          in pkgs.dockerTools.buildImage {
            name = name;
            tag  = "latest";
            copyToRoot = rootfs;
            config = {
              User = "1000";
              WorkingDir = "/app";
              Volumes = { "/data" = {}; };
              ExposedPorts = { "80/tcp" = {}; };
              Entrypoint = [ "${built.${name}}/bin/${name}" ];
              Cmd = [ "--index" "/data/index.db" "--format" "sqlite" "--host" "0.0.0.0" ];
            };
          };

        images = builtins.listToAttrs (map (n: {
          name = "${n}-image";
          value = mkImage n;
        }) members);

        devShell = devenv.lib.mkShell {
          pkgs = pkgs;
          inputs = flakeInputs;
          modules = [ ./devenv.nix ];
        };
      in {
        devShells.default = devShell;

        packages = (built // images // {
          default = built.askld;
        });
      });
}

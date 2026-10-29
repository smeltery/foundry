{
  description = "Foundry development toolchain";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/08d3a97586f6b6343b3b5e799980773629432610";
    rust-overlay = {
      url = "github:oxalica/rust-overlay/8087ff1f47fff983a1fba70fa88b759f2fd8ae97";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    runtimes.url = "github:NixOS/nixpkgs/01fbdeef22b76df85ea168fbfe1bfd9e63681b30";
    python.url = "github:NixOS/nixpkgs/634df81a6bde9521cc2c14bc512ba5b2a89ae13f";
    utilities.url = "github:NixOS/nixpkgs/def71860b28ab804d787a6bcd53b8cfa98c2b7c1";
    hooks.url = "github:NixOS/nixpkgs/2be1525b287109fdbbd1d9e47b90abf73d8547af";
  };

  outputs = inputs:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forSystem = system:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };
          runtimes = import inputs.runtimes { inherit system; };
          pythonPackages = pkgs.callPackage
            (inputs.python + "/pkgs/development/interpreters/python/default.nix") { };
          python = pythonPackages.python313.override { self = python; };
          utilities = import inputs.utilities { inherit system; };
          hooks = import inputs.hooks { inherit system; };
          target = pkgs.stdenv.hostPlatform.rust.rustcTarget;
          cargoTarget = builtins.replaceStrings [ "-" ] [ "_" ]
            (pkgs.lib.toUpper target);
        in {
          rust = pkgs.rust-bin.stable."1.95.0".minimal.override {
            extensions = [ "clippy" "rustfmt" ];
          };
          bun = runtimes.bun;
          nodejs = runtimes.callPackage ./nodejs.nix { };
          inherit python;
          pre-commit = hooks.pre-commit;
          cargo-deny = utilities.cargo-deny;
          actionlint = utilities.actionlint;
          inherit (pkgs) shellcheck git pkg-config;
          cc = pkgs.stdenv.cc;
          binutils = pkgs.stdenv.cc.bintools;
          toolchain-env = pkgs.writeTextDir "share/foundry/toolchain.sh" ''
            export CC=${pkgs.stdenv.cc}/bin/cc
            export CXX=${pkgs.stdenv.cc}/bin/c++
            export AR=${pkgs.stdenv.cc.bintools}/bin/ar
            export RANLIB=${pkgs.stdenv.cc.bintools}/bin/ranlib
            export CARGO_TARGET_${cargoTarget}_LINKER="$CC"
            ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export DEVELOPER_DIR=${pkgs.apple-sdk}
              export SDKROOT="$DEVELOPER_DIR/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"
              export MACOSX_DEPLOYMENT_TARGET=14.0
              export CARGO_TARGET_${cargoTarget}_RUSTFLAGS="-L native=${pkgs.libiconv}/lib"
            ''}
          '';
        };
    in {
      packages = inputs.nixpkgs.lib.genAttrs systems forSystem;
    };
}

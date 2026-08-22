{
  description = "A Nix Flake for a bare-metal Rust Operating System";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      utils,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        cargoBuild = target: pkgs.stdenv.mkDerivation {
          pname = "saeos-${target}";
          version = "0.1.0";
          src = ./.;

          nativeBuildInputs = with pkgs; [
            rustToolchain
            nasm
            binutils
          ];

          dontConfigure = true;
          dontFixup = true;

          buildPhase = ''
            runHook preBuild
            export CARGO_HOME="$TMPDIR/cargo-home"
            mkdir -p "$CARGO_HOME"
            cargo build --offline --locked --target ${target}
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            ${if target == "x86_64-unknown-none" then ''
              objcopy -O elf32-i386 \
                target/${target}/debug/saeos \
                "$out/saeos.bin"
              cp target/${target}/debug/saeos "$out/saeos.elf"
            '' else ''
              mkdir -p "$out/EFI/BOOT"
              cp target/${target}/debug/saeos.efi "$out/EFI/BOOT/BOOTX64.EFI"
            ''}
            runHook postInstall
          '';
        };

        bios = cargoBuild "x86_64-unknown-none";
        uefi = cargoBuild "x86_64-unknown-uefi";

        biosApp = pkgs.writeShellApplication {
          name = "saeos-bios";
          runtimeInputs = [ pkgs.qemu ];
          text = ''
            exec qemu-system-x86_64 \
              -kernel ${bios}/saeos.bin \
              -serial stdio \
              "$@"
          '';
        };

        uefiApp = pkgs.writeShellApplication {
          name = "saeos-uefi";
          runtimeInputs = [ pkgs.qemu ];
          text = ''
            esp_dir="$(mktemp -d -t saeos-uefi-esp.XXXXXX)"
            trap 'rm -rf "$esp_dir"' EXIT
            cp -R ${uefi}/. "$esp_dir/"

            exec qemu-system-x86_64 \
              -bios ${pkgs.OVMF.fd}/FV/OVMF.fd \
              -drive format=raw,file=fat:rw:"$esp_dir" \
              -serial stdio \
              -no-reboot \
              "$@"
          '';
        };
      in
      {
        packages = {
          inherit bios uefi;
          default = bios;
        };

        apps = {
          bios = {
            type = "app";
            program = "${biosApp}/bin/saeos-bios";
          };
          uefi = {
            type = "app";
            program = "${uefiApp}/bin/saeos-uefi";
          };
          default = {
            type = "app";
            program = "${biosApp}/bin/saeos-bios";
          };
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchain
            qemu
            nasm
            binutils
            llvm
            gnumake
            gdb
          ];

          shellHook = ''
            echo "🦀 Rust OS Development Environment Loaded!"
            echo "Target toolchain: $(rustc --version)"
          '';
        };
      }
    );
}

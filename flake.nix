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
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        cargoManifest = builtins.fromTOML (builtins.readFile ./kernel/Cargo.toml);
        packageMetadata = cargoManifest.package;

        cargoBuild =
          target:
          rustPlatform.buildRustPackage {
            pname = "${packageMetadata.name}-${target}";
            version = packageMetadata.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = with pkgs; [
              nasm
              binutils
            ];

            dontUseCargoParallelTests = true;
            dontFixup = true;
            doCheck = false;
            auditable = false;

            buildPhase = ''
              runHook preBuild
              features=""
              if [ -f .config.cargo.toml ]; then
                features="$(
                  sed -n 's/^[[:space:]]*"\([^"]*\)",[[:space:]]*$/\1/p' .config.cargo.toml \
                    | paste -sd, -
                )"
              fi
              feature_args=()
              if [ -n "$features" ]; then
                feature_args=(--features "$features")
              fi
              cargo build --frozen --locked -p ${packageMetadata.name} --target ${target} "''${feature_args[@]}"
              # The initramfs ships next to the kernel, not inside it.
              cargo run --frozen --locked -p mkinitramfs -- build initramfs.toml -o initramfs.cpio
              runHook postBuild
            '';

            installPhase = ''
              runHook preInstall
              mkdir -p "$out"
              ${
                if target == "x86_64-unknown-none" then
                  ''
                    objcopy -O elf32-i386 \
                      target/${target}/debug/${packageMetadata.name} \
                      "$out/saeos.bin"
                    cp target/${target}/debug/${packageMetadata.name} "$out/saeos.elf"
                    cp initramfs.cpio "$out/initramfs.cpio"
                  ''
                else
                  ''
                    mkdir -p "$out/EFI/BOOT"
                    cp target/${target}/debug/${packageMetadata.name}.efi "$out/EFI/BOOT/BOOTX64.EFI"
                    mkdir -p "$out/saeos"
                    cp initramfs.cpio "$out/saeos/initramfs.cpio"
                  ''
              }
              runHook postInstall
            '';
          };

        bios = cargoBuild "x86_64-unknown-none";
        uefi = cargoBuild "x86_64-unknown-uefi";

        biosApp = pkgs.writeShellApplication {
          name = "saeos-bios";
          runtimeInputs = [ pkgs.qemu ];
          # CMDLINE="loglevel=debug" nix run .#bios
          text = ''
            exec qemu-system-x86_64 \
              -kernel ${bios}/saeos.bin \
              -initrd ${bios}/initramfs.cpio \
              -append "''${CMDLINE:-}" \
              -serial stdio \
              -no-reboot \
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
            chmod -R u+w "$esp_dir"
            if [ -n "''${CMDLINE:-}" ]; then
              printf '%s\n' "$CMDLINE" > "$esp_dir/saeos/cmdline.txt"
            fi

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
            # The UEFI runner needs firmware; keep a user-provided path.
            export OVMF_PATH="''${OVMF_PATH:-${pkgs.OVMF.fd}/FV/OVMF.fd}"
            echo "Rust OS Development Environment Loaded!"
            echo "Target toolchain: $(rustc --version)"
          '';
        };
      }
    );
}

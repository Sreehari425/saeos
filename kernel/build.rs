use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let boot_obj = out_dir.join("boot.o");
    let boot_asm = manifest_dir.join("src/arch/x86_64/boot.asm");
    let linker_script = manifest_dir.join("src/arch/x86_64/linker.ld");

    let target = env::var("TARGET").unwrap_or_default();

    if target == "x86_64-unknown-none" {
        // 1. Assemble boot.asm with nasm into OUT_DIR
        let status = Command::new("nasm")
            .arg("-f")
            .arg("elf64")
            .arg(&boot_asm)
            .arg("-o")
            .arg(&boot_obj)
            .status()
            .expect("Failed to execute nasm");

        if !status.success() {
            panic!("nasm failed with exit code: {:?}", status.code());
        }

        // 2. Link boot.o directly into the final binary
        println!("cargo:rustc-link-arg={}", boot_obj.display());

        // 3. Pass linker script and flags
        println!("cargo:rustc-link-arg=-T{}", linker_script.display());
        println!("cargo:rustc-link-arg=-n");
        println!("cargo:rustc-link-arg=-no-pie");
    }

    // 4. Rebuild if boot.asm or linker.ld changes
    println!("cargo:rerun-if-changed=src/arch/x86_64/boot.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/linker.ld");
}

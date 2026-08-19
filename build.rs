use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let boot_obj = out_dir.join("boot.o");

    // 1. Assemble boot.asm with nasm into OUT_DIR
    let status = Command::new("nasm")
        .args(["-f", "elf64", "src/boot.asm", "-o"])
        .arg(&boot_obj)
        .status()
        .expect("Failed to execute nasm");

    if !status.success() {
        panic!("nasm failed with exit code: {:?}", status.code());
    }

    // 2. Link boot.o directly into the final binary
    println!("cargo:rustc-link-arg={}", boot_obj.display());

    // 3. Pass linker script and flags
    println!("cargo:rustc-link-arg=-Tsrc/linker.ld");
    println!("cargo:rustc-link-arg=-n");
    println!("cargo:rustc-link-arg=-no-pie");

    // 4. Rebuild if boot.asm or linker.ld changes
    println!("cargo:rerun-if-changed=src/boot.asm");
    println!("cargo:rerun-if-changed=src/linker.ld");
}

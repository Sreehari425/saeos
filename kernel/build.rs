use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap_or_default();
    let arch_dir = manifest_dir.join("src/arch/x86_64");
    let context_dir = arch_dir.join("context");

    println!("cargo:rerun-if-changed=src/arch/x86_64/boot.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/linker.ld");
    println!("cargo:rerun-if-changed=src/arch/x86_64/context/switch.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/context/interrupt.asm");

    let (nasm_format, object_extension) = match target.as_str() {
        "x86_64-unknown-none" => ("elf64", "o"),
        "x86_64-unknown-uefi" => ("win64", "obj"),
        _ => return,
    };

    for source in ["switch.asm", "interrupt.asm"] {
        let source_path = context_dir.join(source);
        let object_path = out_dir.join(source.replace(".asm", object_extension));
        assemble_nasm(nasm_format, &source_path, &object_path);
        println!("cargo:rustc-link-arg={}", object_path.display());
    }

    if target == "x86_64-unknown-none" {
        let boot_obj = out_dir.join("boot.o");
        assemble_nasm(nasm_format, &arch_dir.join("boot.asm"), &boot_obj);
        println!("cargo:rustc-link-arg={}", boot_obj.display());

        let linker_script = arch_dir.join("linker.ld");
        println!("cargo:rustc-link-arg=-T{}", linker_script.display());
        println!("cargo:rustc-link-arg=-n");
        println!("cargo:rustc-link-arg=-no-pie");
    }
}

fn assemble_nasm(format: &str, source: &std::path::Path, object: &std::path::Path) {
    let mut command = Command::new("nasm");
    command.arg("-f").arg(format);
    let status = command
        .arg(source)
        .arg("-o")
        .arg(object)
        .status()
        .unwrap_or_else(|error| panic!("failed to run nasm for {}: {error}", source.display()));

    if !status.success() {
        panic!(
            "nasm failed for {} with exit code: {:?}",
            source.display(),
            status.code()
        );
    }
}

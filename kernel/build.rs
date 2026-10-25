use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    build_initramfs(&manifest_dir.join("../initramfs"), &out_dir);
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

/// Pack `source` (plus generated test fixtures under `test/`) into a
/// deterministic ustar archive, `initramfs.tar`, and write a manifest of
/// every file's size and FNV-1a hash for the boot test. No external `tar`:
/// entries are sorted and all metadata is fixed, so builds are reproducible.
fn build_initramfs(source: &std::path::Path, out_dir: &std::path::Path) {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut directories: Vec<String> = Vec::new();
    println!("cargo:rerun-if-changed={}", source.display());
    if source.is_dir() {
        collect(source, source, &mut files, &mut directories);
    }
    // Awkward sizes around the 512-byte tar block, empty and nested files.
    directories.extend(["test", "test/a", "test/a/b"].map(String::from));
    for (path, size) in [
        ("test/empty", 0usize),
        ("test/one", 1),
        ("test/block-511", 511),
        ("test/block-512", 512),
        ("test/block-513", 513),
        ("test/large", 64 * 1024 + 1),
        ("test/a/b/nested", 3000),
    ] {
        let data = (0..size).map(|index| (index * 7 + size) as u8).collect();
        files.push((path.to_string(), data));
    }
    directories.sort();
    directories.dedup();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut archive = Vec::new();
    for directory in &directories {
        tar_entry(&mut archive, directory, b'5', &[]);
    }
    let mut manifest = String::from("&[\n");
    for (path, data) in &files {
        tar_entry(&mut archive, path, b'0', data);
        manifest.push_str(&format!(
            "    (\"/{}\", {}, {:#x}),\n",
            path,
            data.len(),
            fnv1a(data)
        ));
    }
    // End of archive: two zero blocks.
    archive.extend_from_slice(&[0; 1024]);
    manifest.push(']');
    std::fs::write(out_dir.join("initramfs.tar"), archive).expect("write initramfs.tar");
    std::fs::write(out_dir.join("initramfs_manifest.rs"), manifest)
        .expect("write initramfs manifest");
}

fn collect(
    root: &std::path::Path,
    directory: &std::path::Path,
    files: &mut Vec<(String, Vec<u8>)>,
    directories: &mut Vec<String>,
) {
    let mut entries: Vec<_> = std::fs::read_dir(directory)
        .expect("read initramfs directory")
        .map(|entry| entry.expect("initramfs entry").path())
        .collect();
    entries.sort();
    for path in entries {
        println!("cargo:rerun-if-changed={}", path.display());
        let relative = path
            .strip_prefix(root)
            .expect("initramfs path")
            .to_str()
            .expect("initramfs paths must be UTF-8")
            .replace('\\', "/");
        if path.is_dir() {
            directories.push(relative);
            collect(root, &path, files, directories);
        } else {
            files.push((relative, std::fs::read(&path).expect("read initramfs file")));
        }
    }
}

/// Append one ustar header (and data, padded to 512 bytes).
fn tar_entry(archive: &mut Vec<u8>, path: &str, kind: u8, data: &[u8]) {
    let mut header = [0u8; 512];
    let (prefix, name) = match path.len() {
        0..=100 => ("", path),
        _ => {
            let split = path[..path.len().min(156)]
                .rfind('/')
                .filter(|&index| path.len() - index - 1 <= 100)
                .unwrap_or_else(|| panic!("initramfs path too long for ustar: {path}"));
            (&path[..split], &path[split + 1..])
        }
    };
    header[..name.len()].copy_from_slice(name.as_bytes());
    let octal = |field: &mut [u8], value: u64| {
        let text = format!("{:0width$o}\0", value, width = field.len() - 1);
        field.copy_from_slice(text.as_bytes());
    };
    octal(
        &mut header[100..108],
        if kind == b'5' { 0o755 } else { 0o644 },
    );
    octal(&mut header[108..116], 0);
    octal(&mut header[116..124], 0);
    octal(&mut header[124..136], data.len() as u64);
    octal(&mut header[136..148], 0);
    header[156] = kind;
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    // The checksum is computed with its own field filled with spaces.
    header[148..156].fill(b' ');
    let checksum: u64 = header.iter().map(|&byte| u64::from(byte)).sum();
    let text = format!("{:06o}\0 ", checksum);
    header[148..156].copy_from_slice(text.as_bytes());
    archive.extend_from_slice(&header);
    archive.extend_from_slice(data);
    archive.resize(archive.len().next_multiple_of(512), 0);
}

fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

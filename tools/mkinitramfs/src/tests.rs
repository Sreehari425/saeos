use super::*;
use std::path::Path;

/// A fresh, empty scratch directory per test.
fn scratch(name: &str) -> std::path::PathBuf {
    let directory =
        std::env::temp_dir().join(format!("mkinitramfs-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory
}

fn write(path: &Path, data: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, data).unwrap();
}

const TREE: &str = "[[tree]]\nsource = \"root\"\ntarget = \"/\"\n";

#[test]
fn builds_are_reproducible_and_ignore_mtimes_and_creation_order() {
    let first = scratch("repro-a");
    let second = scratch("repro-b");
    // Same contents, created in opposite orders with different mtimes.
    let files = [
        ("root/etc/hello", "hi\n"),
        ("root/b/c/d", "deep"),
        ("root/a", ""),
    ];
    for (path, data) in files {
        write(&first.join(path), data.as_bytes());
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    for (path, data) in files.iter().rev() {
        write(&second.join(path), data.as_bytes());
    }
    let build = |base: &Path| {
        let mut image = manifest::parse(TREE, base).unwrap();
        image.add_test_fixtures().unwrap();
        image.to_cpio()
    };
    let archive = build(&first);
    assert_eq!(archive, build(&first), "same inputs, same bytes");
    assert_eq!(archive, build(&second), "independent of order and mtimes");
}

#[test]
fn archive_layout_round_trips() {
    let base = scratch("layout");
    write(&base.join("root/etc/hello"), b"hello\n");
    write(&base.join("bin-init"), b"\x7fELF");
    let text =
        format!("{TREE}[[file]]\nsource = \"bin-init\"\ntarget = \"/bin/init\"\nmode = 0o755\n");
    let image = manifest::parse(&text, &base).unwrap();
    let archive = image.to_cpio();
    assert_eq!(archive.len() % 4, 0);
    let members = cpio::read(&archive).unwrap();
    let summary: Vec<_> = members
        .iter()
        .map(|member| (member.path.as_str(), member.mode, member.data.as_slice()))
        .collect();
    assert_eq!(
        summary,
        [
            ("bin", S_IFDIR | 0o755, &b""[..]),
            ("bin/init", S_IFREG | 0o755, b"\x7fELF"),
            ("etc", S_IFDIR | 0o755, b""),
            ("etc/hello", S_IFREG | 0o644, b"hello\n"),
        ]
    );
    // Header fields are what Linux expects: inode numbers in order, fixed
    // owner and mtime, namesize counting the NUL.
    assert_eq!(&archive[..6], b"070701");
    let field = |index: usize| &archive[6 + index * 8..14 + index * 8];
    assert_eq!(field(0), b"00000001", "ino");
    assert_eq!(field(2), b"00000000", "uid");
    assert_eq!(field(5), b"00000000", "mtime");
    assert_eq!(field(11), b"00000004", "namesize of `bin`");
}

#[test]
fn fixtures_manifest_matches_the_fixture_files() {
    let mut image = Image::default();
    image.add_test_fixtures().unwrap();
    let members = cpio::read(&image.to_cpio()).unwrap();
    let manifest = members
        .iter()
        .find(|member| member.path == "test/MANIFEST")
        .unwrap();
    let lines = std::str::from_utf8(&manifest.data).unwrap().lines();
    let mut checked = 0;
    for line in lines {
        let fields: Vec<_> = line.split(' ').collect();
        let member = members
            .iter()
            .find(|member| format!("/{}", member.path) == fields[0])
            .unwrap();
        assert_eq!(member.data.len().to_string(), fields[1]);
        assert_eq!(format!("{:016x}", fnv1a(&member.data)), fields[2]);
        checked += 1;
    }
    assert_eq!(checked, 7);
    assert!(image.add_test_fixtures().is_err(), "/test is reserved");
}

#[test]
fn bad_manifests_are_rejected() {
    let base = scratch("errors");
    write(&base.join("root/f"), b"x");
    write(&base.join("file"), b"y");
    let cases = [
        (
            "[[tree]]\nsource = \"missing\"\ntarget = \"/\"\n",
            "not a directory",
        ),
        (
            "[[file]]\nsource = \"missing\"\ntarget = \"/x\"\n",
            "missing",
        ),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"relative\"\n",
            "absolute",
        ),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"/a/../b\"\n",
            "`..`",
        ),
        ("[[file]]\nsource = \"file\"\ntarget = \"/a//b\"\n", "empty"),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"/\"\n",
            "cannot be a file",
        ),
        (
            "[[tree]]\nsource = \"root\"\ntarget = \"/\"\n[[file]]\nsource = \"file\"\ntarget = \"/f\"\n",
            "listed twice",
        ),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"/f\"\n[[file]]\nsource = \"file\"\ntarget = \"/f/g\"\n",
            "both a file and a directory",
        ),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"/f\"\nmode = 0o100644\n",
            "mode",
        ),
        (
            "[[file]]\nsource = \"file\"\ntarget = \"/f\"\nowner = 0\n",
            "unknown key",
        ),
        (
            "[[link]]\nsource = \"file\"\ntarget = \"/f\"\n",
            "unknown section",
        ),
        ("tree = 1\n", "array of tables"),
        ("[[tree]]\ntarget = \"/\"\n", "missing string `source`"),
        ("[[tree]\n", ""),
    ];
    for (text, expected) in cases {
        let error = manifest::parse(text, &base).unwrap_err();
        assert!(error.contains(expected), "{text:?}: {error}");
    }
}

#[cfg(unix)]
#[test]
fn symlinks_and_non_utf8_names_are_rejected() {
    use std::os::unix::ffi::OsStrExt;
    let base = scratch("special");
    write(&base.join("root/f"), b"x");
    std::os::unix::fs::symlink("f", base.join("root/link")).unwrap();
    let error = manifest::parse(TREE, &base).unwrap_err();
    assert!(error.contains("only files and directories"), "{error}");

    let other = scratch("non-utf8");
    let name = std::ffi::OsStr::from_bytes(b"bad-\xff");
    write(&other.join("root").join(name), b"x");
    let error = manifest::parse(TREE, &other).unwrap_err();
    assert!(error.contains("non-UTF-8"), "{error}");
}

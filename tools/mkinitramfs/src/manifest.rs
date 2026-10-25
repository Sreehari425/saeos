//! `initramfs.toml`: what goes into the image.
//!
//! ```toml
//! [[tree]]              # copy a directory tree
//! source = "initramfs"  # relative to the manifest's directory
//! target = "/"
//!
//! [[file]]              # one file
//! source = "target/userspace/init"
//! target = "/bin/init"
//! mode = 0o755          # optional, default 0o644
//! test = true           # optional: only in the test image (--test-fixtures)
//! ```

use crate::Image;
use std::path::{Path, PathBuf};

const DEFAULT_FILE_MODE: u32 = 0o644;

/// Which entries to include: the normal image, or the test image (which
/// also gets the `test = true` entries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavour {
    Normal,
    Test,
}

/// Build the image described by the manifest at `path`.
pub fn load(path: &Path, flavour: Flavour) -> Result<Image, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let base = path.parent().unwrap_or(Path::new("."));
    parse(&text, base, flavour).map_err(|error| format!("{}: {error}", path.display()))
}

/// Build the image from manifest `text`; sources are relative to `base`.
pub fn parse(text: &str, base: &Path, flavour: Flavour) -> Result<Image, String> {
    let table: toml::Table = text.parse().map_err(|error| format!("{error}"))?;
    let mut image = Image::default();
    for (key, value) in &table {
        let items = value
            .as_array()
            .ok_or_else(|| format!("`{key}` must be an array of tables ([[{key}]])"))?;
        for item in items {
            let item = item
                .as_table()
                .ok_or_else(|| format!("`{key}` entries must be tables"))?;
            let source = base.join(string(item, "source")?);
            let target = string(item, "target")?;
            match key.as_str() {
                "tree" => {
                    reject_unknown(item, &["source", "target"])?;
                    add_tree(&mut image, &source, target)?;
                }
                "file" => {
                    reject_unknown(item, &["source", "target", "mode", "test"])?;
                    let test_only = match item.get("test") {
                        None => false,
                        Some(value) => value.as_bool().ok_or("`test` must be true or false")?,
                    };
                    if test_only && flavour == Flavour::Normal {
                        continue;
                    }
                    let mode = match item.get("mode") {
                        None => DEFAULT_FILE_MODE,
                        Some(mode) => mode
                            .as_integer()
                            .and_then(|mode| u32::try_from(mode).ok())
                            .filter(|&mode| mode <= 0o7777)
                            .ok_or("`mode` must be a permission number such as 0o755")?,
                    };
                    let data = std::fs::read(&source)
                        .map_err(|error| format!("{}: {error}", source.display()))?;
                    image.add_file(target, mode, data)?;
                }
                other => return Err(format!("unknown section `[[{other}]]`")),
            }
        }
    }
    Ok(image)
}

fn string<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, String> {
    table
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("missing string `{key}`"))
}

fn reject_unknown(table: &toml::Table, allowed: &[&str]) -> Result<(), String> {
    match table.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("unknown key `{key}`")),
        None => Ok(()),
    }
}

/// Add every directory and file below `source` at `target`. Entries are
/// sorted by the image, so directory iteration order does not matter.
fn add_tree(image: &mut Image, source: &Path, target: &str) -> Result<(), String> {
    if !source.is_dir() {
        return Err(format!("{}: not a directory", source.display()));
    }
    image.add_directory(target)?;
    let mut pending: Vec<(PathBuf, String)> = vec![(source.to_path_buf(), target.to_string())];
    while let Some((directory, target)) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
            let path = entry.path();
            let name = entry.file_name().into_string().map_err(|name| {
                format!("non-UTF-8 file name {name:?} in {}", directory.display())
            })?;
            let child = format!("{}/{name}", target.trim_end_matches('/'));
            let kind = entry
                .file_type()
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if kind.is_dir() {
                image.add_directory(&child)?;
                pending.push((path, child));
            } else if kind.is_file() {
                let data =
                    std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                image.add_file(&child, DEFAULT_FILE_MODE, data)?;
            } else {
                return Err(format!(
                    "{}: only files and directories are supported",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

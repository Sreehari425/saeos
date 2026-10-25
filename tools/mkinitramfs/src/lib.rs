//! Build the SaeOS initramfs: a deterministic cpio newc archive from a
//! manifest (`initramfs.toml`). Entries are sorted and all metadata (owner,
//! timestamps, inode numbers) is fixed, so the same inputs always give the
//! same bytes.

pub mod cpio;
pub mod manifest;

use cpio::{Member, S_IFDIR, S_IFREG};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Directory,
    File { mode: u32, data: Vec<u8> },
}

/// The image being built, keyed by relative path (`etc/hello`). A
/// `BTreeMap` sorts parents before their children.
#[derive(Debug, Default, Clone)]
pub struct Image {
    nodes: BTreeMap<String, Node>,
}

/// Turn an absolute target (`/etc/hello`) into the archive's relative form.
fn relative(target: &str) -> Result<String, String> {
    let rest = target
        .strip_prefix('/')
        .ok_or_else(|| format!("target `{target}` must be absolute"))?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if !rest.is_empty()
        && rest
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err(format!(
            "target `{target}` must not contain empty, `.` or `..` components"
        ));
    }
    Ok(rest.to_string())
}

impl Image {
    /// Add a directory (and its parents). Adding one twice is allowed, so
    /// several trees can share directories.
    pub fn add_directory(&mut self, target: &str) -> Result<(), String> {
        let path = relative(target)?;
        if path.is_empty() {
            return Ok(());
        }
        self.add_parents(&path)?;
        match self.nodes.get(&path) {
            Some(Node::Directory) => Ok(()),
            Some(Node::File { .. }) => Err(format!("`/{path}` is both a file and a directory")),
            None => {
                self.nodes.insert(path, Node::Directory);
                Ok(())
            }
        }
    }

    /// Add a file (and its parent directories). Each file may be added once.
    pub fn add_file(&mut self, target: &str, mode: u32, data: Vec<u8>) -> Result<(), String> {
        let path = relative(target)?;
        if path.is_empty() {
            return Err("`/` cannot be a file".to_string());
        }
        self.add_parents(&path)?;
        if self.nodes.contains_key(&path) {
            return Err(format!("`/{path}` is listed twice"));
        }
        self.nodes.insert(path, Node::File { mode, data });
        Ok(())
    }

    fn add_parents(&mut self, path: &str) -> Result<(), String> {
        match path.rfind('/') {
            Some(index) => self.add_directory(&format!("/{}", &path[..index])),
            None => Ok(()),
        }
    }

    /// Generated files for the boot test: sizes around block and page
    /// boundaries, an empty file, a nested file, and `/test/MANIFEST` listing
    /// `path size fnv1a-hex` for each of them, so the kernel test reads its
    /// expectations from the image it boots with.
    /// Fails if any fixture path is already taken.
    pub fn add_test_fixtures(&mut self) -> Result<(), String> {
        let mut manifest = String::new();
        for (path, size) in [
            ("/test/empty", 0usize),
            ("/test/one", 1),
            ("/test/block-511", 511),
            ("/test/block-512", 512),
            ("/test/block-513", 513),
            ("/test/large", 64 * 1024 + 1),
            ("/test/a/b/nested", 3000),
        ] {
            let data: Vec<u8> = (0..size).map(|index| (index * 7 + size) as u8).collect();
            manifest.push_str(&format!("{path} {size} {:016x}\n", fnv1a(&data)));
            self.add_file(path, 0o644, data)?;
        }
        self.add_file("/test/MANIFEST", 0o644, manifest.into_bytes())
    }

    /// Archive members in path order: parents before children.
    pub fn members(&self) -> Vec<Member> {
        self.nodes
            .iter()
            .map(|(path, node)| match node {
                Node::Directory => Member {
                    path: path.clone(),
                    mode: S_IFDIR | 0o755,
                    data: Vec::new(),
                },
                Node::File { mode, data } => Member {
                    path: path.clone(),
                    mode: S_IFREG | mode,
                    data: data.clone(),
                },
            })
            .collect()
    }

    pub fn to_cpio(&self) -> Vec<u8> {
        cpio::write(&self.members())
    }
}

/// FNV-1a 64, the hash the boot test checks fixtures with.
pub fn fnv1a(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests;

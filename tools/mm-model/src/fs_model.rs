//! Model-based random test for the VFS on ramfs.
//!
//! A trivial reference model (`BTreeMap<path, node>`) implements the same
//! Linux rules independently. Thousands of random operations run against
//! both; every result, including which errno a failure returns, must match,
//! and the whole tree (contents and listings) is compared periodically.

use crate::fs::ramfs::RamFs;
use crate::fs::vfs::Vfs;
use crate::fs::{Errno, FileTable, InodeKind, OpenFlags, Whence};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    File(Vec<u8>),
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Done,
    Bytes(Vec<u8>),
    Stat(InodeKind, u64),
}

type Result = std::result::Result<Outcome, Errno>;

struct Model {
    nodes: BTreeMap<String, Node>,
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(index) => &path[..index],
        None => "/",
    }
}

fn components(path: &str) -> Vec<&str> {
    path.split('/').filter(|part| !part.is_empty()).collect()
}

impl Model {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), Node::Directory);
        Self { nodes }
    }

    /// Path walk: each prefix must be a directory, then the next must exist.
    fn resolve(&self, path: &str) -> std::result::Result<&Node, Errno> {
        let mut current = String::from("/");
        for part in components(path) {
            if self.nodes.get(&current) != Some(&Node::Directory) {
                return Err(Errno::ENOTDIR);
            }
            current = if current == "/" {
                format!("/{part}")
            } else {
                format!("{current}/{part}")
            };
            if !self.nodes.contains_key(&current) {
                return Err(Errno::ENOENT);
            }
        }
        Ok(&self.nodes[&current])
    }

    /// The parent must resolve to a directory; `/` has no parent entry.
    fn parent(&self, path: &str) -> std::result::Result<(), Errno> {
        if path == "/" {
            return Err(Errno::EBUSY);
        }
        match self.resolve(parent_of(path))? {
            Node::Directory => Ok(()),
            Node::File(_) => Err(Errno::ENOTDIR),
        }
    }

    fn children(&self, directory: &str) -> Vec<String> {
        self.nodes
            .keys()
            .filter(|path| path.as_str() != "/" && parent_of(path) == directory)
            .map(|path| path.rsplit('/').next().unwrap().to_string())
            .collect()
    }

    fn mkdir(&mut self, path: &str) -> Result {
        match self.parent(path) {
            Err(Errno::EBUSY) => return Err(Errno::EEXIST),
            result => result?,
        }
        if self.nodes.contains_key(path) {
            return Err(Errno::EEXIST);
        }
        self.nodes.insert(path.to_string(), Node::Directory);
        Ok(Outcome::Done)
    }

    /// `open` semantics; returns the path of the opened node.
    fn open(&mut self, path: &str, flags: OpenFlags) -> std::result::Result<(), Errno> {
        match self.resolve(path) {
            Ok(node) => {
                if flags.contains(OpenFlags::CREATE | OpenFlags::EXCL) {
                    return Err(Errno::EEXIST);
                }
                if *node == Node::Directory
                    && (flags.contains(OpenFlags::WRITE) || flags.contains(OpenFlags::TRUNC))
                {
                    return Err(Errno::EISDIR);
                }
            }
            Err(Errno::ENOENT) if flags.contains(OpenFlags::CREATE) => {
                self.parent(path)?;
                self.nodes.insert(path.to_string(), Node::File(Vec::new()));
            }
            Err(error) => return Err(error),
        }
        if flags.contains(OpenFlags::TRUNC)
            && flags.contains(OpenFlags::WRITE)
            && let Some(Node::File(data)) = self.nodes.get_mut(path)
        {
            data.clear();
        }
        Ok(())
    }

    fn create(&mut self, path: &str, flags: OpenFlags) -> Result {
        self.open(path, flags).map(|_| Outcome::Done)
    }

    fn write(&mut self, path: &str, offset: usize, bytes: &[u8]) -> Result {
        self.open(path, OpenFlags::WRITE)?;
        let Some(Node::File(data)) = self.nodes.get_mut(path) else {
            unreachable!("opened for writing");
        };
        if data.len() < offset + bytes.len() {
            data.resize(offset + bytes.len(), 0);
        }
        data[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(Outcome::Done)
    }

    fn read(&mut self, path: &str, offset: usize, length: usize) -> Result {
        self.open(path, OpenFlags::READ)?;
        match &self.nodes[path] {
            Node::Directory => Err(Errno::EISDIR),
            Node::File(data) => {
                let start = offset.min(data.len());
                let end = (offset + length).min(data.len());
                Ok(Outcome::Bytes(data[start..end].to_vec()))
            }
        }
    }

    fn truncate(&mut self, path: &str, size: usize) -> Result {
        self.open(path, OpenFlags::WRITE)?;
        if let Some(Node::File(data)) = self.nodes.get_mut(path) {
            data.resize(size, 0);
        }
        Ok(Outcome::Done)
    }

    fn unlink(&mut self, path: &str) -> Result {
        match self.parent(path) {
            Err(Errno::EBUSY) => return Err(Errno::EISDIR),
            result => result?,
        }
        match self.nodes.get(path) {
            None => Err(Errno::ENOENT),
            Some(Node::Directory) => Err(Errno::EISDIR),
            Some(Node::File(_)) => {
                self.nodes.remove(path);
                Ok(Outcome::Done)
            }
        }
    }

    fn rmdir(&mut self, path: &str) -> Result {
        self.parent(path)?;
        match self.nodes.get(path) {
            None => Err(Errno::ENOENT),
            Some(Node::File(_)) => Err(Errno::ENOTDIR),
            Some(Node::Directory) if !self.children(path).is_empty() => Err(Errno::ENOTEMPTY),
            Some(Node::Directory) => {
                self.nodes.remove(path);
                Ok(Outcome::Done)
            }
        }
    }

    fn rename(&mut self, old: &str, new: &str) -> Result {
        self.parent(old)?;
        self.parent(new)?;
        if old == new {
            return self
                .nodes
                .get(old)
                .map(|_| Outcome::Done)
                .ok_or(Errno::ENOENT);
        }
        let source = self.nodes.get(old).cloned().ok_or(Errno::ENOENT)?;
        let new_parent = parent_of(new);
        if source == Node::Directory
            && (new_parent == old || new_parent.starts_with(&format!("{old}/")))
        {
            return Err(Errno::EINVAL);
        }
        if let Some(existing) = self.nodes.get(new) {
            match (&source, existing) {
                (Node::Directory, Node::Directory) if !self.children(new).is_empty() => {
                    return Err(Errno::ENOTEMPTY);
                }
                (Node::Directory, Node::File(_)) => return Err(Errno::ENOTDIR),
                (Node::File(_), Node::Directory) => return Err(Errno::EISDIR),
                _ => {}
            }
            self.nodes.remove(new);
        }
        let prefix = format!("{old}/");
        let moved: Vec<String> = self
            .nodes
            .keys()
            .filter(|path| path.as_str() == old || path.starts_with(&prefix))
            .cloned()
            .collect();
        for path in moved {
            let node = self.nodes.remove(&path).unwrap();
            let renamed = format!("{new}{}", &path[old.len()..]);
            self.nodes.insert(renamed, node);
        }
        Ok(Outcome::Done)
    }

    fn stat(&self, path: &str) -> Result {
        Ok(match self.resolve(path)? {
            Node::Directory => Outcome::Stat(InodeKind::Directory, 0),
            Node::File(data) => Outcome::Stat(InodeKind::File, data.len() as u64),
        })
    }
}

/// The same operations through the real VFS and file descriptors.
struct Real {
    vfs: Vfs,
    files: FileTable,
}

impl Real {
    fn new() -> Self {
        let vfs = Vfs::new();
        vfs.mount("/", RamFs::new()).unwrap();
        Self {
            vfs,
            files: FileTable::new(),
        }
    }

    fn with_fd<T>(
        &self,
        path: &str,
        flags: OpenFlags,
        operation: impl FnOnce(u32) -> std::result::Result<T, Errno>,
    ) -> std::result::Result<T, Errno> {
        let fd = self.vfs.open(&self.files, path, flags)?;
        let result = operation(fd);
        self.vfs.close(&self.files, fd).unwrap();
        result
    }

    fn create(&self, path: &str, flags: OpenFlags) -> Result {
        self.with_fd(path, flags, |_| Ok(Outcome::Done))
    }

    fn write(&self, path: &str, offset: usize, bytes: &[u8]) -> Result {
        self.with_fd(path, OpenFlags::WRITE, |fd| {
            self.vfs
                .lseek(&self.files, fd, offset as i64, Whence::Set)?;
            assert_eq!(self.vfs.write(&self.files, fd, bytes)?, bytes.len());
            Ok(Outcome::Done)
        })
    }

    fn read(&self, path: &str, offset: usize, length: usize) -> Result {
        self.with_fd(path, OpenFlags::READ, |fd| {
            self.vfs
                .lseek(&self.files, fd, offset as i64, Whence::Set)?;
            let mut buffer = vec![0; length];
            let mut filled = 0;
            // At least one call, even for 0 bytes: reading a directory is
            // EISDIR regardless of length.
            loop {
                let count = self.vfs.read(&self.files, fd, &mut buffer[filled..])?;
                filled += count;
                if count == 0 || filled == length {
                    break;
                }
            }
            buffer.truncate(filled);
            Ok(Outcome::Bytes(buffer))
        })
    }

    fn truncate(&self, path: &str, size: usize) -> Result {
        self.with_fd(path, OpenFlags::WRITE, |fd| {
            self.vfs.ftruncate(&self.files, fd, size as u64)?;
            Ok(Outcome::Done)
        })
    }

    fn stat(&self, path: &str) -> Result {
        let stat = self.vfs.stat(path)?;
        Ok(Outcome::Stat(stat.kind, stat.size))
    }

    fn listing(&self, path: &str) -> Vec<String> {
        self.with_fd(path, OpenFlags::READ | OpenFlags::DIRECTORY, |fd| {
            Ok(self
                .vfs
                .readdir(&self.files, fd)?
                .into_iter()
                .map(|entry| entry.name)
                .collect())
        })
        .unwrap()
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn random_path(rng: &mut Rng) -> String {
    const NAMES: [&str; 4] = ["a", "b", "c", "d"];
    let depth = 1 + rng.below(3) as usize;
    let mut path = String::new();
    for _ in 0..depth {
        path.push('/');
        path.push_str(NAMES[rng.below(4) as usize]);
    }
    path
}

fn assert_same_tree(model: &Model, real: &Real, context: &str) {
    for (path, node) in &model.nodes {
        match node {
            Node::Directory => {
                assert_eq!(
                    real.listing(path),
                    model.children(path),
                    "{context}: listing of {path}"
                );
            }
            Node::File(data) => {
                assert_eq!(
                    real.read(path, 0, data.len() + 1),
                    Ok(Outcome::Bytes(data.clone())),
                    "{context}: contents of {path}"
                );
            }
        }
    }
}

const SEEDS: &[u64] = if cfg!(miri) {
    &[3]
} else {
    &[1, 0xfeed, 0xdead_beef, 42, 0x5ae05]
};
const STEPS: usize = if cfg!(miri) { 300 } else { 20_000 };
const CHECK_EVERY: usize = if cfg!(miri) { 50 } else { 100 };

#[test]
fn vfs_matches_reference_model() {
    for &seed in SEEDS {
        let mut rng = Rng(seed);
        let mut model = Model::new();
        let real = Real::new();
        for step in 0..STEPS {
            let path = random_path(&mut rng);
            let (description, expected, actual) = match rng.below(100) {
                0..=14 => (
                    format!("mkdir {path}"),
                    model.mkdir(&path),
                    real.vfs.mkdir(&path).map(|_| Outcome::Done),
                ),
                15..=29 => {
                    let flags = [
                        OpenFlags::WRITE | OpenFlags::CREATE,
                        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCL,
                        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNC,
                        OpenFlags::READ,
                    ][rng.below(4) as usize];
                    (
                        format!("open {path} {flags:?}"),
                        model.create(&path, flags),
                        real.create(&path, flags),
                    )
                }
                30..=49 => {
                    let offset = rng.below(2_000) as usize;
                    let bytes: Vec<u8> = (0..rng.below(300)).map(|_| rng.next() as u8).collect();
                    (
                        format!("write {path} @{offset} len {}", bytes.len()),
                        model.write(&path, offset, &bytes),
                        real.write(&path, offset, &bytes),
                    )
                }
                50..=64 => {
                    let offset = rng.below(2_500) as usize;
                    let length = rng.below(700) as usize;
                    (
                        format!("read {path} @{offset} len {length}"),
                        model.read(&path, offset, length),
                        real.read(&path, offset, length),
                    )
                }
                65..=71 => {
                    let size = rng.below(3_000) as usize;
                    (
                        format!("truncate {path} {size}"),
                        model.truncate(&path, size),
                        real.truncate(&path, size),
                    )
                }
                72..=79 => (
                    format!("unlink {path}"),
                    model.unlink(&path),
                    real.vfs.unlink(&path).map(|_| Outcome::Done),
                ),
                80..=86 => (
                    format!("rmdir {path}"),
                    model.rmdir(&path),
                    real.vfs.rmdir(&path).map(|_| Outcome::Done),
                ),
                87..=94 => {
                    let target = random_path(&mut rng);
                    (
                        format!("rename {path} {target}"),
                        model.rename(&path, &target),
                        real.vfs.rename(&path, &target).map(|_| Outcome::Done),
                    )
                }
                _ => (format!("stat {path}"), model.stat(&path), real.stat(&path)),
            };
            assert_eq!(actual, expected, "seed {seed} step {step}: {description}");
            if step % CHECK_EVERY == 0 {
                assert_same_tree(&model, &real, &format!("seed {seed} step {step}"));
            }
        }
        assert_same_tree(&model, &real, &format!("seed {seed} end"));
        assert_eq!(real.files.open_count(), 0, "every descriptor was closed");
    }
}

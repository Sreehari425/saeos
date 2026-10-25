//! Relative paths: each process has a current directory, and every path a
//! syscall receives is resolved against it into a clean absolute path.

use alloc::string::String;
use alloc::vec::Vec;

/// `path` (absolute, or relative to `cwd`) as a normalised absolute path:
/// no empty, `.` or `..` components, `..` at the root stays at the root.
/// The VFS does the same when walking; this form is what `getcwd` returns.
pub fn resolve(cwd: &str, path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    let start = if path.starts_with('/') { "" } else { cwd };
    for component in start.split('/').chain(path.split('/')) {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            component => components.push(component),
        }
    }
    let mut resolved = String::from("/");
    resolved.push_str(&components.join("/"));
    resolved
}

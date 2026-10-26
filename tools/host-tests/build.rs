// The kernel's task and memory modules are compiled into this host crate via
// `#[path]`. `host_model` tells them they are running in the model rather
// than the kernel, independent of the target triple.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(host_model)");
    println!("cargo::rustc-cfg=host_model");
}

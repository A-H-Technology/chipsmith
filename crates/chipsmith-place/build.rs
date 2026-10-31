// With the `gpu` feature, CubeCL 0.11.3's cubecl-llvm emits the bundled
// LLVM's static libraries before pliron-llvm, which calls into them, on
// the link line; GNU ld scans each archive once and leaves
// LLVM_InitializeNativeTarget and LLVMOrc* undefined. lld resolves
// regardless of order (experiments/gpu-bench/.cargo/config.toml and crates/chipsmith/build.rs, the same
// workaround). No upstream issue yet; drop this once the default linker
// links a `--features gpu` build.
fn main() {
    println!("cargo::rerun-if-env-changed=CARGO_FEATURE_GPU");
    if std::env::var_os("CARGO_FEATURE_GPU").is_some() {
        println!("cargo::rustc-link-arg=-fuse-ld=lld");
    }
}

fn main() {
    // CUDA FFI bindings
    println!("cargo:rustc-link-lib=dylib=cuda");
    println!("cargo:rustc-link-search=native=/usr/lib");
}

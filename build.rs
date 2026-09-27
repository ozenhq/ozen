// screencapturekit's Swift bridge links the Swift runtime (libswift_Concurrency etc.); find it in the OS copy.
fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
}

// screencapturekit's Swift bridge links the Swift runtime (libswift_Concurrency etc.); find it in the OS copy.
// The locate binary carries an Info.plist so macOS sees it as Ozen (see locate.plist).
fn main() {
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    let plist = concat!(env!("CARGO_MANIFEST_DIR"), "/locate.plist");
    println!("cargo:rustc-link-arg-bin=locate=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
    println!("cargo:rerun-if-changed=locate.plist");
}

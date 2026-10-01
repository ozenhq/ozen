// The locate binary carries an Info.plist so macOS sees it as Ozen (see locate.plist).
fn main() {
    let plist = concat!(env!("CARGO_MANIFEST_DIR"), "/locate.plist");
    println!("cargo:rustc-link-arg-bin=locate=-Wl,-sectcreate,__TEXT,__info_plist,{plist}");
    println!("cargo:rerun-if-changed=locate.plist");
}

fn main() {
    println!("cargo:rerun-if-changed=assets/windows/app.rc");
    println!("cargo:rerun-if-changed=assets/windows/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    embed_resource::compile("assets/windows/app.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}

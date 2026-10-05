fn main() {
    println!("cargo:rerun-if-changed=assets/windows.rc");
    println!("cargo:rerun-if-changed=assets/AppIcon.ico");
    println!("cargo:rerun-if-env-changed=CARGO_PKG_VERSION");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for(
            "assets/windows.rc",
            ["ducklocal"],
            embed_resource::ParamsIncludeDirs(["assets"]),
        )
        .manifest_required()
        .expect("Cannot embed the Windows application icon; install the Windows SDK");
    }
}

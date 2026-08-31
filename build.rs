fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            // GPUI loads the executable's icon resource with numeric ID 1 on Windows.
            .set_icon_with_id("assets/app-icon.ico", "1")
            .compile()
            .expect("failed to embed the Windows application icon");
    }
}

fn main() {
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=assets/i18n");
    println!("cargo:rerun-if-changed=assets/resources.rc");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    println!("cargo:rerun-if-changed=assets/app.ico");

    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_bundled_translations("assets/i18n");

    slint_build::compile_with_config("ui/app.slint", config)
        .expect("Failed to compile Slint UI with Fluent style");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/resources.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Failed to compile Windows resources and manifest");
    }
}

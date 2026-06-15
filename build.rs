fn main() {
    // Force the "fluent" widget style. Without this, slint-build can fall back
    // to a style under which std-widgets text inputs (LineEdit/TextEdit) render
    // invisibly here, while the slint! macro defaulted to fluent and worked.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".to_string());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
}

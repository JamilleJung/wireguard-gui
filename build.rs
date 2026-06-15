fn main() {
    // Force the "fluent" widget style. Without this, slint-build can fall back
    // to a style under which std-widgets text inputs (LineEdit/TextEdit) render
    // invisibly here, while the slint! macro defaulted to fluent and worked.
    // fluent-light: force the light palette so std-widgets use dark-text-on-light
    // fills (matching our white windows). Without this, GNOME dark mode gives the
    // inputs near-white fills that vanish on a white background.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-light".to_string());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
}

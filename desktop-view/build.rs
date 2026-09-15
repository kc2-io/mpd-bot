fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("../ui/desktop.slint", config).expect("compile desktop UI");
}

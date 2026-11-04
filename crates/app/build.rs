fn main() {
    // Estilo fixo para a interface ser idêntica em Windows, Linux e macOS.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("falha ao compilar a interface Slint");
}

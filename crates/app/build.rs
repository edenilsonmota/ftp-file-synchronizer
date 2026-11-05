fn main() {
    // Estilo fixo para a interface ser idêntica em Windows, Linux e macOS.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        // Os testes de interface localizam elementos pelo rótulo de acessibilidade,
        // o que exige informação de depuração; o executável de release fica sem ela.
        .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"));
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("falha ao compilar a interface Slint");
}

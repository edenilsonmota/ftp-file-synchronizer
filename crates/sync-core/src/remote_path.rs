//! Manipulação de caminhos remotos (sempre no formato POSIX, com `/`).

use std::path::{Component, Path};

use crate::{Error, Result};

/// Normaliza um caminho remoto: barras `/`, prefixo `/`, sem `.`, sem barras
/// duplicadas e sem barra final (exceto a raiz).
pub fn normalize(path: &str) -> String {
    let parts: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    format!("/{}", parts.join("/"))
}

/// Une a pasta remota base com um caminho relativo local, recusando `..`.
pub fn join(remote_base: &str, relative: &Path) -> Result<String> {
    if remote_base.split(['/', '\\']).any(|part| part == "..") {
        return Err(Error::invalid("Caminho remoto não pode conter \"..\"."));
    }
    let mut parts = vec![normalize(remote_base)];
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return Err(Error::invalid("Caminho remoto inválido.")),
        }
    }
    Ok(normalize(&parts.join("/")))
}

/// Une uma pasta remota com o nome de uma subpasta.
pub fn child(base: &str, name: &str) -> String {
    normalize(&format!("{base}/{name}"))
}

/// Pasta pai de um caminho remoto (a raiz é pai de si mesma).
pub fn parent(path: &str) -> String {
    let normalized = normalize(path);
    match normalized.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => normalized[..index].to_string(),
    }
}

/// Último segmento de um caminho remoto.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn normaliza_caminhos() {
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
        assert_eq!(normalize("public_html/"), "/public_html");
        assert_eq!(normalize("//a///b/./c/"), "/a/b/c");
        assert_eq!(normalize("\\web\\site"), "/web/site");
    }

    #[test]
    fn une_base_com_relativo_de_qualquer_sistema() {
        let relative: PathBuf = ["assets", "img", "logo.png"].iter().collect();
        assert_eq!(
            join("/public_html/", &relative).unwrap(),
            "/public_html/assets/img/logo.png"
        );
        assert_eq!(join("/", Path::new("index.html")).unwrap(), "/index.html");
    }

    #[test]
    fn recusa_saida_da_pasta_remota() {
        assert!(join("/public_html/../private", Path::new("index.html")).is_err());
        assert!(join("/public_html", Path::new("../index.html")).is_err());
    }

    #[test]
    fn navega_entre_pastas() {
        assert_eq!(child("/", "web"), "/web");
        assert_eq!(child("/web", "site"), "/web/site");
        assert_eq!(parent("/web/site"), "/web");
        assert_eq!(parent("/web"), "/");
        assert_eq!(parent("/"), "/");
        assert_eq!(file_name("/web/index.html"), "index.html");
    }
}

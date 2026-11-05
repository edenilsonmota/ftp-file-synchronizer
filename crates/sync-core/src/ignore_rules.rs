//! Padrões de arquivos ignorados, com a mesma sintaxe do `.gitignore`.
//!
//! As regras globais (preferências) vêm antes das regras do projeto, então um
//! projeto pode reincluir algo com `!padrão`.

use std::fs;
use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::{Error, Message, Result};

/// Padrões globais usados quando o usuário ainda não definiu os seus.
pub const DEFAULT_PATTERNS: [&str; 4] = ["node_modules/", ".git/", ".vscode/", "desktop.ini"];

pub struct IgnoreRules {
    matcher: Gitignore,
}

impl IgnoreRules {
    pub fn new(root: &Path, global: &[String], project: &[String]) -> Result<Self> {
        let mut builder = GitignoreBuilder::new(root);
        // Segue o sistema de arquivos: Windows e macOS não diferenciam maiúsculas.
        let invalid = |error: ignore::Error| {
            Error::invalid(Message::InvalidIgnorePattern {
                reason: error.to_string(),
            })
        };
        builder
            .case_insensitive(cfg!(any(windows, target_os = "macos")))
            .map_err(invalid)?;
        for pattern in global.iter().chain(project) {
            builder.add_line(None, pattern).map_err(invalid)?;
        }
        Ok(Self {
            matcher: builder.build().map_err(invalid)?,
        })
    }

    /// `path` é absoluto, dentro da pasta do projeto.
    pub fn is_ignored(&self, root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        if relative.as_os_str().is_empty() {
            return false;
        }
        let matches = |is_dir| {
            self.matcher
                .matched_path_or_any_parents(relative, is_dir)
                .is_ignore()
        };
        match fs::metadata(path) {
            Ok(metadata) => matches(metadata.is_dir()),
            // Caminho apagado: o tipo é desconhecido, então vale qualquer padrão
            // (nunca remove do servidor algo que o usuário mandou ignorar).
            Err(_) => matches(false) || matches(true),
        }
    }
}

/// Converte o texto digitado (um padrão por linha) em lista, sem linhas vazias.
pub fn parse_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(root: &Path, global: &[&str], project: &[&str]) -> IgnoreRules {
        let owned = |list: &[&str]| list.iter().map(|item| item.to_string()).collect::<Vec<_>>();
        IgnoreRules::new(root, &owned(global), &owned(project)).unwrap()
    }

    #[test]
    fn padroes_padrao_ignoram_pastas_de_ferramentas() {
        let root = Path::new("/p");
        let rules = rules(root, &DEFAULT_PATTERNS, &[]);
        assert!(rules.is_ignored(root, Path::new("/p/node_modules/x/index.js")));
        assert!(rules.is_ignored(root, Path::new("/p/sub/.git/HEAD")));
        assert!(rules.is_ignored(root, Path::new("/p/desktop.ini")));
        assert!(!rules.is_ignored(root, Path::new("/p/.gitignore")));
        assert!(!rules.is_ignored(root, Path::new("/p/src/git.js")));
        assert!(!rules.is_ignored(root, Path::new("/p")));
    }

    #[test]
    fn projeto_soma_e_reinclui_padroes() {
        let root = Path::new("/p");
        let rules = rules(
            root,
            &["*.log"],
            &["/config.php", "storage/", "!importante.log"],
        );
        assert!(rules.is_ignored(root, Path::new("/p/erro.log")));
        assert!(!rules.is_ignored(root, Path::new("/p/importante.log")));
        assert!(rules.is_ignored(root, Path::new("/p/config.php")));
        assert!(!rules.is_ignored(root, Path::new("/p/modulos/config.php")));
        assert!(rules.is_ignored(root, Path::new("/p/storage/cache/a.txt")));
    }

    /// Caminhos são relativos à pasta local do projeto; com `/` no início ou no
    /// meio, o padrão vale a partir da raiz do projeto (como no Git).
    #[test]
    fn caminhos_relativos_a_raiz_do_projeto() {
        let root = Path::new("/home/dev/professor");
        let rules = rules(
            root,
            &[],
            &["application/controllers/config/*", "**/cache/*", "/.env"],
        );
        let path = |relative: &str| root.join(relative);
        assert!(rules.is_ignored(root, &path("application/controllers/config/db.php")));
        // `*` não atravessa pastas, mas a pasta inteira já está coberta.
        assert!(rules.is_ignored(root, &path("application/controllers/config/sub/a.php")));
        // Com barra no meio, só a partir da raiz do projeto.
        assert!(!rules.is_ignored(root, &path("modulos/application/controllers/config/a.php")));
        assert!(!rules.is_ignored(root, &path("application/controllers/Home.php")));
        // `**/` vale em qualquer profundidade.
        assert!(rules.is_ignored(root, &path("cache/a.html")));
        assert!(rules.is_ignored(root, &path("modulos/x/cache/a.html")));
        assert!(rules.is_ignored(root, &path(".env")));
        assert!(!rules.is_ignored(root, &path("api/.env")));
        // Caminho completo do disco ou do servidor não funciona como padrão.
        let absolute = rules_for(root, "/home/dev/professor/application/");
        assert!(!absolute.is_ignored(root, &path("application/a.php")));
    }

    fn rules_for(root: &Path, pattern: &str) -> IgnoreRules {
        rules(root, &[], &[pattern])
    }

    #[test]
    fn pasta_ignorada_apagada_continua_ignorada() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rules = rules(root, &[], &["build/"]);
        // `build` não existe: pode ter sido uma pasta apagada.
        assert!(rules.is_ignored(root, &root.join("build")));
    }

    #[test]
    fn padrao_invalido_vira_erro() {
        let result = IgnoreRules::new(Path::new("/p"), &["{a,b".to_string()], &[]);
        assert!(result.is_err());
    }

    #[test]
    fn converte_texto_em_padroes() {
        assert_eq!(
            parse_patterns("*.log\n\n  \nvendor/  \n# comentário\n"),
            vec!["*.log", "vendor/", "# comentário"]
        );
    }
}

//! Garante uma única instância: a segunda execução pede à primeira que
//! mostre a janela e encerra.

use std::io::{BufRead, BufReader, Write};
use std::thread;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, Stream};

const SHOW_COMMAND: &str = "show";

fn socket_name() -> std::io::Result<Name<'static>> {
    // Inclui o usuário: em Windows os named pipes são globais à máquina.
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    format!("{}-{user}.sock", sync_core::APP_ID).to_ns_name::<GenericNamespaced>()
}

/// Se já existe uma instância, pede que ela apareça e retorna `true`.
pub fn notify_existing() -> bool {
    let Ok(name) = socket_name() else {
        return false;
    };
    match Stream::connect(name) {
        Ok(mut stream) => {
            let _ = stream.write_all(format!("{SHOW_COMMAND}\n").as_bytes());
            true
        }
        Err(_) => false,
    }
}

/// Escuta pedidos de outras instâncias em segundo plano.
pub fn listen(on_show: impl Fn() + Send + 'static) {
    let listener = match socket_name().and_then(|name| {
        ListenerOptions::new()
            .name(name)
            .try_overwrite(true)
            .create_sync()
    }) {
        Ok(listener) => listener,
        Err(error) => {
            log::warn!("Instância única indisponível: {error}");
            return;
        }
    };
    let spawned = thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            for connection in listener.incoming().filter_map(Result::ok) {
                let mut line = String::new();
                if BufReader::new(connection).read_line(&mut line).is_ok()
                    && line.trim() == SHOW_COMMAND
                {
                    on_show();
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("Instância única indisponível: {error}");
    }
}

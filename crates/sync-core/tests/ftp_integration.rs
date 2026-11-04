//! Testes contra um servidor FTP real (libunftp) rodando dentro do teste.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use libunftp::ServerBuilder;
use sync_core::Config;
use sync_core::ftp::{self, FtpClient};
use unftp_sbe_fs::Filesystem;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Sobe o servidor em segundo plano servindo `root` e retorna a configuração para acessá-lo.
fn start_server(root: PathBuf) -> Config {
    let port = free_port();
    let passive = free_port();
    thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let server =
                ServerBuilder::new(Box::new(move || Filesystem::new(root.clone()).unwrap()))
                    .passive_ports(passive..=passive.saturating_add(20))
                    .build()
                    .unwrap();
            let _ = server.listen(format!("127.0.0.1:{port}")).await;
        });
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "servidor FTP não subiu");
        thread::sleep(Duration::from_millis(50));
    }
    Config {
        host: "127.0.0.1".into(),
        user: "anonymous".into(),
        password: "teste".into(),
        port,
        ..Config::default()
    }
}

#[test]
fn envia_lista_e_remove_no_servidor() {
    let server_root = tempfile::tempdir().unwrap();
    let local = tempfile::tempdir().unwrap();
    let config = start_server(server_root.path().to_path_buf());

    ftp::test_connection(&config).unwrap();

    // Bytes com \n e \r\n: em modo ASCII seriam alterados no envio.
    let content = b"linha1\nlinha2\r\n\x00\xff".to_vec();
    let file = local.path().join("logo.bin");
    fs::write(&file, &content).unwrap();

    let mut client = FtpClient::default();
    client.connect(&config).unwrap();
    client
        .upload(&config, &file, "/site/assets/img/logo.bin")
        .unwrap();
    client.upload(&config, &file, "/site/index.bin").unwrap();
    assert_eq!(
        fs::read(server_root.path().join("site/assets/img/logo.bin")).unwrap(),
        content
    );

    let listing = ftp::list_directories(&config, "/site").unwrap();
    assert_eq!(listing.current_path, "/site");
    assert_eq!(listing.directories, vec!["assets".to_string()]);
    let fallback = ftp::list_directories(&config, "/nao-existe").unwrap();
    assert_eq!(fallback.current_path, "/");

    assert!(client.remove(&config, "/site/index.bin").unwrap());
    assert!(!server_root.path().join("site/index.bin").exists());

    assert!(client.remove(&config, "/site/assets").unwrap());
    assert!(!server_root.path().join("site/assets").exists());

    assert!(!client.remove(&config, "/site/nao-existe.txt").unwrap());

    // Depois de remover pastas, um novo envio recria a estrutura.
    client
        .upload(&config, &file, "/site/assets/img/logo.bin")
        .unwrap();
    assert!(server_root.path().join("site/assets/img/logo.bin").exists());
    client.disconnect();
}

#[test]
fn servidor_inacessivel_falha() {
    let config = Config {
        host: "127.0.0.1".into(),
        user: "u".into(),
        port: free_port(),
        ..Config::default()
    };
    assert!(ftp::test_connection(&config).is_err());
}

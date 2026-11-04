# FTP File Synchronizer

> Ferramenta de sincronização FTP automática, agnóstica a editor.
> **Versão atual:** 3.0.0

O **FTP File Synchronizer** é um aplicativo desktop leve que monitora pastas locais e envia automaticamente arquivos alterados para um servidor FTP. Funciona com Zed, Cursor, VS Code, Sublime Text, Notepad++ ou qualquer outro editor.

O aplicativo é escrito em **Rust** com interface em **[Slint](https://slint.dev)**: sem navegador embutido, com instaladores de cerca de 10 MB e baixo consumo de memória.

## Funcionalidades

- monitoramento e sincronização de arquivos em tempo real;
- sincronização somente de alterações ocorridas após iniciar o monitoramento;
- espelhamento de exclusões locais no servidor;
- envio do conteúdo de pastas criadas ou renomeadas;
- múltiplos mapeamentos local/remoto, com navegador de pastas do FTP;
- FTP ou FTPS explícito (certificados validados pelo sistema operacional);
- senha guardada no cofre do sistema (Windows Credential Manager, Keychain do macOS ou Secret Service no Linux);
- instância única: abrir o aplicativo novamente traz a janela existente;
- suporte a Windows, Linux (X11 e Wayland) e macOS.

## Instalação

Acesse a página de [Releases](https://github.com/edenilsonmota/ftp-file-synchronizer/releases) e baixe o pacote do seu sistema:

- **Windows:** `FTP File Synchronizer_x.x.x_x64-setup.exe`;
- **Linux:** `ftp-file-synchronizer_x.x.x_x86_64.AppImage`, `ftp-file-synchronizer_x.x.x_amd64.deb` ou `ftp-file-synchronizer-x.x.x-1-x86_64.pkg.tar.zst`;
- **macOS:** `.dmg` para Apple Silicon (`aarch64`) ou Intel (`x64`).

### Linux: instalar e desinstalar

Escolha apenas um dos formatos abaixo.

#### AppImage

Não requer instalação no sistema. Dê permissão de execução e abra o arquivo:

```bash
chmod +x ftp-file-synchronizer_x.x.x_x86_64.AppImage
./ftp-file-synchronizer_x.x.x_x86_64.AppImage
```

Para desinstalar, apague o arquivo AppImage que foi baixado.

#### Debian/Ubuntu (`.deb`)

```bash
sudo apt install ./ftp-file-synchronizer_x.x.x_amd64.deb
```

Para desinstalar:

```bash
sudo apt remove ftp-file-synchronizer
```

#### Arch Linux (`.pkg.tar.zst`)

```bash
sudo pacman -U ftp-file-synchronizer-x.x.x-1-x86_64.pkg.tar.zst
```

Para desinstalar, incluindo dependências que ficaram sem uso:

```bash
sudo pacman -Rns ftp-file-synchronizer
```

Em todos os formatos, a desinstalação mantém as configurações já salvas pelo aplicativo.

### macOS

Enquanto o aplicativo não é assinado e notarizado pela Apple, o macOS bloqueia a primeira abertura. Depois de copiar o app para **Aplicativos**, libere-o com:

```bash
xattr -dr com.apple.quarantine "/Applications/FTP File Synchronizer.app"
```

## Como usar

1. Preencha host, usuário, senha e porta do FTP (marque **FTPS** se o servidor suportar).
2. Adicione um projeto selecionando a pasta local e informando a pasta remota, como `/public_html/site`, ou escolha-a em **Escolher FTP**.
3. Clique em **INICIAR**.
4. Trabalhe normalmente no seu editor; as mudanças serão sincronizadas.

Ao minimizar, o aplicativo permanece na barra de tarefas. Ao fechar a janela, o aplicativo e a sincronização são encerrados e a configuração é salva.

Os itens `node_modules`, `.git`, `.vscode` e `desktop.ini` são ignorados. Se a pasta do projeto ficar indisponível (disco desmontado, unidade de rede desconectada), as exclusões não são espelhadas no servidor.

## Arquitetura

O projeto é um workspace Cargo com dois crates:

- `crates/sync-core`: motor de sincronização, sem dependência de interface:
  - `config.rs`: persistência da configuração, cofre de senhas;
  - `validate.rs`: validação da configuração;
  - `ftp.rs`: cliente FTP/FTPS;
  - `engine.rs`: observador de pastas, fila e envio;
  - `remote_path.rs`: caminhos remotos.
- `crates/app`: aplicativo desktop:
  - `ui/app.slint`: interface;
  - `src/main.rs`: ligação entre interface e motor;
  - `src/single_instance.rs`: instância única.

Os eventos do sistema de arquivos variam entre Windows, Linux e macOS. Por isso a fila guarda apenas *qual caminho mudou* e a ação é decidida ao processar, olhando o estado atual do disco: arquivo existente gera upload, caminho inexistente gera remoção remota, pasta criada ou renomeada gera o envio do seu conteúdo.

### Ícones no Linux

O nome do binário, o `app_id` da janela no Wayland, o `WM_CLASS` no X11, o arquivo `ftp-file-synchronizer.desktop` e os ícones em `/usr/share/icons/hicolor/<tamanho>/apps/ftp-file-synchronizer.png` usam o mesmo identificador (`sync_core::APP_ID`). Manter todos iguais é o que permite a qualquer ambiente (GNOME, KDE, Hyprland e outros) associar a janela ao ícone.

## Desenvolvimento

### Pré-requisitos

- Rust estável, instalado via [rustup](https://rustup.rs);
- no Linux, bibliotecas de desenvolvimento:
  - Debian/Ubuntu: `sudo apt install libfontconfig1-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev libdbus-1-dev`;
  - Arch: `sudo pacman -S fontconfig libxkbcommon wayland`.

Para editar a interface com pré-visualização ao vivo, instale a extensão do Slint no seu editor (VS Code, Zed e outros).

### Comandos

```bash
cargo run -p ftp-file-synchronizer        # inicia o aplicativo
cargo test --workspace                     # testes (inclui servidor FTP real e observador de pastas)
cargo clippy --workspace --all-targets     # análise estática
cargo fmt --all                            # formatação
```

### Pacotes locais

```bash
cargo install cargo-packager --locked
cargo build --release -p ftp-file-synchronizer
cargo packager --release -p ftp-file-synchronizer
```

Os pacotes são gravados em `dist/`. No Linux são gerados `.deb`, AppImage e um `PKGBUILD`; em uma máquina Arch, rode `makepkg --nodeps` dentro de `dist/` para obter o `.pkg.tar.zst`. Em distribuições muito recentes, como Arch, gere o AppImage com `NO_STRIP=1` antes do comando.

### Integração contínua e releases

- `.github/workflows/ci.yml`: formatação, Clippy, testes e compilação em Windows, Linux e macOS a cada push e pull request.
- `.github/workflows/release.yml`: ao publicar uma tag `v*`, gera os instaladores dos três sistemas e cria a release no GitHub:

```bash
git tag v3.0.0
git push origin v3.0.0
```

O pacote Linux é compilado no Ubuntu 22.04 para continuar compatível com distribuições mais antigas.

## Agradecimentos

Projeto criado por [Edenilson Mota](https://github.com/edenilsonmota). Interface construída com [Slint](https://slint.dev).

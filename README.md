# Keeper (DownStream)

Um gerenciador de downloads leve e persistente com interface GTK4 e LibAdwaita.

## Características

- ✅ Downloads paralelos em chunks para maior velocidade
- ✅ Pausa e retomada de downloads
- ✅ Persistência de downloads entre sessões
- ✅ Interface moderna com GTK4 e LibAdwaita
- ✅ Badges coloridos para status dos downloads
- ✅ Indicador de tamanho de arquivo
- ✅ Retry automático em caso de falha de conexão

## Requisitos

- Rust 1.70 ou superior
- Cargo (geralmente vem com Rust)
- GTK4 e LibAdwaita (bibliotecas de desenvolvimento)

## Instalação

### Ubuntu/Debian

1. Instale o Rust e Cargo:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

2. Instale as dependências do sistema:
```bash
sudo apt update
sudo apt install -y \
    build-essential \
    pkg-config \
    libgtk-4-dev \
    libadwaita-1-dev \
    libssl-dev \
    libpango1.0-dev \
    libcairo2-dev \
    libgdk-pixbuf-2.0-dev \
    libglib2.0-dev
```

3. Clone o repositório (se ainda não tiver):
```bash
git clone <url-do-repositorio>
cd Keeper
```

4. Compile o projeto:
```bash
cargo build --release
```

5. Execute o aplicativo:
```bash
./target/release/downstream
```

### Fedora

1. Instale o Rust e Cargo:
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

2. Instale as dependências do sistema:
```bash
sudo dnf install -y \
    gcc \
    gcc-c++ \
    pkg-config \
    gtk4-devel \
    libadwaita-devel \
    openssl-devel \
    pango-devel \
    cairo-devel \
    gdk-pixbuf2-devel \
    glib2-devel
```

3. Clone o repositório (se ainda não tiver):
```bash
git clone <url-do-repositorio>
cd Keeper
```

4. Compile o projeto:
```bash
cargo build --release
```

5. Execute o aplicativo:
```bash
./target/release/downstream
```

## Desenvolvimento

Para compilar em modo de desenvolvimento (com símbolos de debug):

```bash
cargo build
```

Para executar em modo de desenvolvimento:

```bash
cargo run
```

## Estrutura do Projeto

- `src/main.rs` - Código principal da aplicação
- `Cargo.toml` - Configuração do projeto e dependências
- `downloads.json` - Arquivo de persistência dos downloads (criado automaticamente em `~/.local/share/keeper/`)

## Notas

- Os downloads são salvos no diretório padrão de downloads do sistema
- O histórico de downloads é persistido em `~/.local/share/keeper/downloads.json`
- A aplicação suporta downloads paralelos quando o servidor suporta Range requests

# ClipForge

**Alternativa open source, local e gratuita ao Opus Clip, escrita em Rust.**
Recebe um vídeo longo (link ou arquivo) e gera clipes curtos e verticais. Cada clipe tem título viral, legendas animadas e enquadramento automático no rosto.

Funciona em **Linux, Windows e macOS**. Licença [MIT](LICENSE).

## Downloads

Baixe a versão mais recente em **[Releases](https://github.com/littlefarm7/clipforge/releases)**.

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (LMDE, Mint, Ubuntu, Debian, Fedora…) | `ClipForge-<versão>-x86_64.AppImage` |
| Windows 10/11 64 bits | `ClipForge-<versão>-windows-x64.zip` |
| macOS com chip Apple (M1–M4) | `ClipForge-<versão>-macos-arm64.zip` |
| macOS com processador Intel | `ClipForge-<versão>-macos-x64.zip` |

No macOS, o app requer a versão 12 ou mais nova. A IA local precisa do macOS 13.3 ou mais novo.

No macOS, o app requer a versão 12 ou mais nova. A IA local precisa do macOS 13.3 ou mais novo.

### Linux
```bash
chmod +x ClipForge-*-x86_64.AppImage
./ClipForge-*-x86_64.AppImage
```

### Windows
1. Extraia o `.zip` para uma pasta.
2. Abra o `ClipForge.exe`.
3. Se o Windows mostrar um aviso, clique em **Mais informações → Executar assim mesmo**.

### macOS
1. Extraia o `.zip` e mova o `ClipForge.app` para **Aplicativos**.
2. Libere o app uma vez no Terminal:
   ```bash
   xattr -dr com.apple.quarantine /Applications/ClipForge.app
   ```
3. Abra o ClipForge.

O app não tem janela própria. A interface abre no navegador em `http://127.0.0.1:7861`. Para fechar, use o botão **Sair** na página.

## Como funciona

1. **Baixa o vídeo** com o yt-dlp (YouTube, Twitch, Vimeo, X, Instagram e mais de 1000 sites) ou recebe um arquivo enviado.
2. **Transcreve** com marcação de tempo por palavra:
   - Local: whisper.cpp (grátis, offline).
   - API: qualquer serviço compatível com OpenAI (Groq gratuito, OpenAI).
3. **Escolhe os melhores trechos e cria um título viral** para cada um:
   - IA local: llama.cpp com um modelo GGUF baixado pelo app (Qwen 2.5, Llama 3.2…).
   - API de IA: Groq, Google Gemini, OpenRouter, OpenAI, Ollama, LM Studio ou outra compatível com OpenAI.
   - Sem IA: uma heurística simples, sem modelo.
4. **Renderiza** cada clipe com o ffmpeg:
   - Formatos 9:16, 1:1, 4:5 ou 16:9.
   - Enquadramento: seguir o rosto, cortar no centro ou fundo desfocado.
   - Título no topo, em uma caixa escura translúcida que não cobre o rosto.
   - Legendas estilo karaokê, com a palavra falada em destaque.

Nada sai do seu computador, a não ser que você escolha usar uma API.

## Primeiro uso

1. Abra **⚙ Configurações → Modelos locais** e baixe:
   - um modelo Whisper (ex.: *Base* ou *Large v3 Turbo*);
   - um modelo de IA (ex.: *Qwen 2.5 3B*).
2. Na aba **🧠 IA**, escolha "IA local" e o modelo baixado. Clique em **Salvar**.
3. Cole um link e clique em **✂ Gerar clipes**.

## Dicas

- **Para trocar o título de um clipe:** clique em **✎ Editar**, mude o título e clique em **Renderizar de novo**.
- **Para esconder o título:** vá em Configurações → Clipes e legendas → Título.
- **YouTube pede login ("não é um robô"):** em Configurações → Sistema, coloque `--cookies-from-browser firefox` (ou `chrome`) nos argumentos extras do yt-dlp.
- **Um site parou de baixar:** clique em **Atualizar yt-dlp** em Configurações → Sistema.
- **PC com pouca RAM:** use Whisper Base e Qwen 2.5 1.5B, ou use uma API.

### Onde ficam os dados

| Sistema | Pasta |
|---|---|
| Linux | `~/.local/share/clipforge` |
| Windows | `%APPDATA%\clipforge` |
| macOS | `~/Library/Application Support/clipforge` |

## Compilar

Para rodar em modo de desenvolvimento, com `ffmpeg` e `yt-dlp` instalados no sistema:
```bash
cargo run --release
```

Para gerar os pacotes das 3 plataformas a partir de um Linux x86_64:
```bash
pip install ziglang==0.13.0
cargo install cargo-zigbuild
cargo install apple-codesign --bin rcodesign   # opcional, assina os binários do macOS
./packaging/build-all.sh                        # ou: ./packaging/build-all.sh linux
```

Ao enviar uma tag `v*` para o GitHub, o workflow `.github/workflows/release.yml` compila tudo e publica a Release.

## Estrutura

| Arquivo | Função |
|---|---|
| `src/main.rs` | Servidor web (axum) e rotas da API |
| `src/pipeline.rs` | Fila e fluxo: baixar → transcrever → analisar → renderizar |
| `src/transcribe.rs` | whisper.cpp local e API de transcrição |
| `src/highlights.rs` | Seleção de trechos e títulos com IA ou heurística |
| `src/llm.rs` | Cliente compatível com OpenAI e llama-server local |
| `src/render.rs` | ffmpeg, recortes, título e legendas ASS animadas |
| `src/face.rs` | Detecção de rosto (rustface) e caminho suave da câmera |
| `src/models.rs` | Catálogo e download de modelos |
| `src/paths.rs` | Pastas de dados e localização das ferramentas em cada sistema |
| `assets/index.html` | Interface |
| `packaging/` | Scripts e arquivos dos pacotes (AppImage, Windows, macOS) |

## Contribuir

Veja [CONTRIBUTING.md](CONTRIBUTING.md). Os componentes de terceiros incluídos nos pacotes e as licenças deles estão em [THIRD_PARTY.md](THIRD_PARTY.md).

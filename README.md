# ClipForge

**Alternativa open source, local e gratuita ao Opus Clip, escrita em Rust.**
Recebe um vídeo longo (link ou arquivo) e gera clipes curtos e verticais. Cada clipe tem legendas animadas, enquadramento automático no rosto e um título sugerido para a postagem.

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
3. **Escolhe os melhores trechos** e sugere um título para postar cada um:
   - IA local: llama.cpp com um modelo GGUF baixado pelo app (Qwen 2.5, Llama 3.2…).
   - API de IA: Groq, Google Gemini, OpenRouter, OpenAI, Ollama, LM Studio ou outra compatível com OpenAI.
   - Sem IA: uma heurística simples, sem modelo.
4. **Renderiza** cada clipe com o ffmpeg:
   - Formatos 9:16, 1:1, 4:5 ou 16:9.
   - Enquadramento automático e pensado para cada clipe:
     - descobre quem está falando (movimento da boca com a cabeça estabilizada + voz);
     - corta para quem fala em podcasts de plano aberto;
     - enquadra duas pessoas juntas quando cabem no quadro;
     - usa tela dividida nos trechos de conversa rápida;
     - respeita os cortes de câmera do vídeo original;
     - mostra o vídeo inteiro quando não há rostos.
     Dá para trocar o enquadramento de cada clipe em ✎ Ajustar.
   - Qualidade de Shorts profissionais: baixa o original em até 4K para o recorte vertical ficar nítido,
     mantém 60 quadros por segundo, compressão de alta qualidade e cores no padrão BT.709.
   - Legendas estilo karaokê, com a palavra falada em destaque.

Nada sai do seu computador, a não ser que você escolha usar uma API.

## Primeiro uso

1. Abra o ClipForge e clique em **Preparar o ClipForge**.
   - O app vê quanta memória o computador tem e escolhe sozinho a qualidade ideal: Leve, Equilibrado ou Máxima.
   - Os arquivos de inteligência artificial são baixados uma única vez. Depois, tudo funciona sem internet.
   - Também dá para usar pela internet (serviço gratuito Groq) ou sem inteligência artificial.
2. Cole um link (ou arraste um arquivo) e clique em **✂ Criar clipes**. Você não precisa esperar o download terminar.

O ClipForge cuida sozinho das partes técnicas:
- **Baixador de vídeos:** fica sempre atualizado.
- **Quando o YouTube pede login:** usa o login que já está no seu navegador.
- **Memória da IA:** é ajustada ao seu computador.
- **Se a IA falhar:** os cortes são escolhidos automaticamente e um aviso explica o motivo.
- **Se algo der errado:** a mensagem é simples e diz o que fazer. Os detalhes técnicos ficam escondidos.

## Dicas

- **Para ajustar um clipe:** clique em **✎ Ajustar**, mude o início, o fim ou o título e clique em **Salvar e gerar de novo**.
- **Para baixar tudo:** use **⬇ Baixar todos**.
- **Legendas:** escolha o estilo (Clássico, Moderno, Neon ou Pop), a posição e o tamanho em ⚙ Ajustes.
- **Quem entende de tecnologia:** encontra outros serviços de IA, a memória de contexto e os argumentos do yt-dlp em ⚙ Ajustes → **Opções avançadas**.

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
| `src/render.rs` | ffmpeg, recortes e legendas ASS animadas |
| `src/face.rs` | Detecção de rosto (rustface) e caminho suave da câmera |
| `src/models.rs` | Catálogo e download de modelos |
| `src/paths.rs` | Pastas de dados e localização das ferramentas em cada sistema |
| `assets/index.html` | Interface |
| `packaging/` | Scripts e arquivos dos pacotes (AppImage, Windows, macOS) |

## Contribuir

Veja [CONTRIBUTING.md](CONTRIBUTING.md). Os componentes de terceiros incluídos nos pacotes e as licenças deles estão em [THIRD_PARTY.md](THIRD_PARTY.md).

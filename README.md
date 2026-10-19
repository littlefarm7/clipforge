# ClipForge

Alternativa local e gratuita ao Opus Clip, escrita em Rust. Recebe um vídeo longo (link ou arquivo) e gera clipes curtos e verticais, com legendas animadas e enquadramento automático no rosto.

## Como funciona

1. **Baixa o vídeo** com o yt-dlp (YouTube, Twitch, Vimeo, X, Instagram e mais de 1000 sites) ou recebe um arquivo enviado.
2. **Transcreve** com marcação de tempo por palavra:
   - Local: whisper.cpp (grátis, offline).
   - API: qualquer serviço compatível com OpenAI (Groq gratuito, OpenAI).
3. **Escolhe os melhores trechos**:
   - IA local: llama.cpp com um modelo GGUF baixado pelo app (Qwen 2.5, Llama 3.2…).
   - API de IA: Groq, Google Gemini, OpenRouter, OpenAI, Ollama, LM Studio ou outra compatível com OpenAI.
   - Sem IA: uma heurística simples, sem modelo.
4. **Renderiza** cada clipe com o ffmpeg:
   - Formatos 9:16, 1:1, 4:5 ou 16:9.
   - Enquadramento: seguir o rosto, cortar no centro ou fundo desfocado.
   - Legendas estilo karaokê, com a palavra falada em destaque.

O app abre uma página no navegador (`http://127.0.0.1:7861`). Nada sai do seu PC, a não ser que você escolha usar uma API.

## Uso rápido (AppImage)

```bash
chmod +x ClipForge-0.1.0-x86_64.AppImage
./ClipForge-0.1.0-x86_64.AppImage
```

1. Abra **⚙ Configurações → Modelos locais** e baixe:
   - um modelo Whisper (ex.: *Base* ou *Large v3 Turbo*);
   - um modelo de IA (ex.: *Qwen 2.5 3B*).
2. Na aba **🧠 IA**, escolha "IA local" e o modelo baixado. Clique em **Salvar**.
3. Cole um link e clique em **✂ Gerar clipes**.

Os projetos ficam em `~/.local/share/clipforge/projects/` e as configurações em `~/.config/clipforge/config.json`.

## Dicas

- **YouTube pede login ("não é um robô")**: em Configurações → Sistema, coloque `--cookies-from-browser firefox` (ou `chrome`) nos argumentos extras do yt-dlp.
- **Um site parou de baixar**: clique em "Atualizar yt-dlp" em Configurações → Sistema.
- **Para editar um clipe**: clique em "✎ Editar", ajuste o início, o fim e o título, depois clique em "Renderizar de novo".
- **Para escolher outros trechos com novas configurações**: clique em "⟳ Gerar de novo". A transcrição é reaproveitada.
- **PC com pouca RAM**: use Whisper Base e Qwen 2.5 1.5B, ou use uma API.
- **CPU antiga sem AVX2**: o app detecta isso e usa automaticamente a versão compatível do whisper e do llama.

## Compilar

```bash
pip install ziglang && cargo install cargo-zigbuild
./packaging/build-appimage.sh
```

## Estrutura

| Arquivo | Função |
|---|---|
| `src/main.rs` | Servidor web (axum) e rotas da API |
| `src/pipeline.rs` | Fila e fluxo: baixar → transcrever → analisar → renderizar |
| `src/transcribe.rs` | whisper.cpp local e API de transcrição |
| `src/highlights.rs` | Seleção de trechos com IA ou heurística |
| `src/llm.rs` | Cliente compatível com OpenAI e llama-server local |
| `src/render.rs` | ffmpeg, recortes e legendas ASS animadas |
| `src/face.rs` | Detecção de rosto (rustface) e caminho suave da câmera |
| `src/models.rs` | Catálogo e download de modelos |
| `assets/index.html` | Interface |

## Licenças dos componentes embutidos

- ffmpeg: GPL
- whisper.cpp e llama.cpp: MIT
- yt-dlp: Unlicense
- deno: MIT
- Fontes Anton e Poppins: OFL
- Modelo de rosto SeetaFace (via rustface): BSD

Os textos das licenças estão em `usr/share/doc/clipforge` dentro do AppImage.

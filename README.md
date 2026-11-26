# KERF

**Corte vídeos longos em clipes que prendem. Open source, local e gratuito, escrito em Rust.**

*Kerf* é o nome do espaço que a lâmina deixa ao cortar. O KERF recebe um vídeo longo (link ou arquivo)
e entrega clipes curtos editados como os canais de cortes profissionais: cortes rápidos, legendas
coloridas por pessoa, zoom de ênfase, telas de live montadas, memes, listas de melhores momentos e
música de fundo. Antes se chamava ClipForge.

Funciona em **Linux, Windows e macOS**. Licença [MIT](LICENSE).

## Downloads

Baixe a versão mais recente em **[Releases](https://github.com/littlefarm7/clipforge/releases)**.

| Sistema | Arquivo |
|---|---|
| Linux x86_64 (LMDE, Mint, Ubuntu, Debian, Fedora…) | `KERF-<versão>-x86_64.AppImage` |
| Windows 10/11 64 bits | `KERF-<versão>-windows-x64.zip` |
| macOS com chip Apple (M1–M4) | `KERF-<versão>-macos-arm64.zip` |
| macOS com processador Intel | `KERF-<versão>-macos-x64.zip` |

No macOS, o app requer a versão 12 ou mais nova. A IA local precisa do macOS 13.3 ou mais novo.

### Linux
```bash
chmod +x KERF-*-x86_64.AppImage
./KERF-*-x86_64.AppImage
```

### Windows
1. Extraia o `.zip` para uma pasta.
2. Abra o `KERF.exe`.
3. Se o Windows mostrar um aviso, clique em **Mais informações → Executar assim mesmo**.

### macOS
1. Extraia o `.zip` e mova o `KERF.app` para **Aplicativos**.
2. Libere o app uma vez no Terminal:
   ```bash
   xattr -dr com.apple.quarantine /Applications/KERF.app
   ```
3. Abra o KERF.

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
4. **Edita** cada clipe na **estrutura de edição** escolhida:

| Estrutura | Para que serve | O que a edição faz |
|---|---|---|
| **Clássico** | Qualquer vídeo | Segue quem fala, junta duas pessoas ou divide a tela; legenda karaokê |
| **Podcast Turbo** | Podcasts e entrevistas | Remove pausas, corta para quem fala, planos de reação, zoom alternado e nas frases fortes, legenda em frases com uma cor por pessoa, palavrões censurados (c4r4lh0), áudio alto |
| **Conversa** | Bate-papo e humor | Mantém as pausas, segura a câmera em quem ri, legenda de frase inteira alinhada à esquerda |
| **React Dividido** | Lives com webcam | Acha a caixa da webcam, coloca em cima; jogo embaixo; título fixo na emenda; karaokê de 2–3 palavras |
| **React Destaque** | Lives com reações | Fundo desfocado, jogo numa faixa; webcam grande na fala e janelinha na ação; título nos 3 primeiros segundos; uma palavra gigante por vez |
| **Cinemático** | Séries, filmes, esquetes | Close no rosto, cor tratada (contraste, vinheta, nitidez), legenda empilhada ao lado do rosto com palavras-chave em azul |
| **Meme** | Momentos engraçados curtos | A IA escreve o texto do meme; caixa branca em cima e o vídeo embaixo |
| **Top Momentos** | Compilações | Junta vários momentos, título fixo, lista numerada digitada, tela preta entre momentos |

   Em todas (menos Clássico) dá para escolher uma **música de fundo** da sua biblioteca: ela abaixa
   sozinha quando alguém fala e termina com fade. Cada estrutura tem as próprias opções (cortar pausas,
   reações, zoom, cor por pessoa, censura, título, cor de cinema, nome da webcam, título da lista…).
   - Formatos 9:16, 1:1, 4:5 ou 16:9.
   - Qualidade de Shorts profissionais: original em até 4K, 60 quadros por segundo quando o vídeo tiver,
     compressão de alta qualidade, cores BT.709 e volume no padrão das plataformas.
   - Marca d'água opcional com o @ do seu perfil, semitransparente.

Nada sai do seu computador, a não ser que você escolha usar uma API.

## Primeiro uso

1. Abra o KERF e clique em **Preparar o KERF**.
   - O app vê quanta memória o computador tem e escolhe sozinho a qualidade ideal: Leve, Equilibrado ou Máxima.
   - Os arquivos de inteligência artificial são baixados uma única vez. Depois, tudo funciona sem internet.
   - Também dá para usar pela internet (serviço gratuito Groq) ou sem inteligência artificial.
2. Escolha a **estrutura de edição** e ajuste as opções dela.
3. Cole um link (ou arraste um arquivo) e clique em **Cortar ✂**. Você não precisa esperar o download terminar.

O KERF cuida sozinho das partes técnicas:
- **Baixador de vídeos:** fica sempre atualizado.
- **Quando o YouTube pede login:** usa o login que já está no seu navegador.
- **Memória da IA:** é ajustada ao seu computador.
- **Se a IA falhar:** os cortes são escolhidos automaticamente e um aviso explica o motivo.
- **Se algo der errado:** a mensagem é simples e diz o que fazer. Os detalhes técnicos ficam escondidos.

## Dicas

- **Para ajustar um clipe:** clique em **Ajustar**. Dá para mudar o início, o fim, o título, a estrutura e o texto do meme, e clicar em **Salvar e gerar de novo**.
- **Para baixar tudo:** use **Baixar todos**.
- **Música de fundo:** envie suas músicas em **Ajustes → Músicas de fundo** (ou pelo botão **Enviar** na estrutura). Use músicas que você tem direito de usar.
- **Tema claro ou escuro:** botão **◐** no topo. Por padrão, segue o sistema.
- **Legendas do Clássico:** estilo, posição e tamanho em **Ajustes**.
- **Marca d'água:** em **Ajustes → Marca d'água**, digite o @ do perfil e escolha a posição.
- **Quem entende de tecnologia:** encontra outros serviços de IA, a memória de contexto e os argumentos do yt-dlp em **Ajustes → Opções avançadas**.

### Onde ficam os dados

| Sistema | Pasta |
|---|---|
| Linux | `~/.local/share/kerf` |
| Windows | `%APPDATA%\kerf` |
| macOS | `~/Library/Application Support/kerf` |

Quem usava o ClipForge não perde nada: na primeira vez, o KERF move as pastas antigas (projetos, modelos,
músicas e ajustes) para as novas.

Para testar uma estrutura sem a interface:
```bash
kerf --estrutura VIDEO TRANSCRICAO.json ESTRUTURA INICIO FIM PASTA [MOMENTOS.json]
```

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
| `src/structures.rs` | Catálogo das estruturas de edição e das opções de cada uma |
| `src/edit.rs` | Motor das estruturas: trechos sem pausas, planos, zoom, layouts de live, música e volume |
| `src/captions.rs` | Legendas de cada estrutura (frase colorida, karaokê, palavra gigante, empilhada, meme, lista) |
| `src/render.rs` | Estrutura Clássico: ffmpeg, recortes e legendas karaokê |
| `src/face.rs` | Detecção de rosto (rustface), quem está falando e caminho suave da câmera |
| `src/models.rs` | Catálogo e download de modelos |
| `src/paths.rs` | Pastas de dados e localização das ferramentas em cada sistema |
| `assets/index.html` | Interface (neo-brutalista, monocromática, claro e escuro) |
| `packaging/` | Scripts e arquivos dos pacotes (AppImage, Windows, macOS) |

## Contribuir

Veja [CONTRIBUTING.md](CONTRIBUTING.md). Os componentes de terceiros incluídos nos pacotes e as licenças deles estão em [THIRD_PARTY.md](THIRD_PARTY.md).

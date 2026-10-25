# Componentes de terceiros

O código do ClipForge é distribuído sob a licença MIT (arquivo `LICENSE`).
Os pacotes prontos (AppImage, zip do Windows e do macOS) também incluem os programas abaixo, cada um com a sua licença:

| Componente | Uso | Licença | Fonte |
|---|---|---|---|
| FFmpeg | Corte, legendas e codificação de vídeo | GPL v3 (builds estáticos com x264) | https://ffmpeg.org |
| whisper.cpp | Transcrição local | MIT | https://github.com/ggml-org/whisper.cpp |
| llama.cpp | IA local (llama-server) | MIT | https://github.com/ggml-org/llama.cpp |
| yt-dlp | Download de vídeos da web | Unlicense | https://github.com/yt-dlp/yt-dlp |
| Deno | Executa os scripts do YouTube usados pelo yt-dlp | MIT | https://github.com/denoland/deno |
| Fontes Anton e Poppins | Legendas e título | SIL Open Font License 1.1 | https://fonts.google.com |
| Modelo SeetaFace (via rustface) | Detecção de rosto (modo "Seguir quem fala") | BSD-2-Clause | https://github.com/atomashpolskiy/rustface |
| Modelo YuNet (OpenCV Zoo) | Detecção de rosto do auto reframe | MIT | https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet |
| tract | Executa o modelo YuNet (ONNX) | MIT ou Apache-2.0 | https://github.com/sonos/tract |
| fast_image_resize | Recorte e redimensionamento dos quadros | MIT ou Apache-2.0 | https://github.com/Cykooz/fast_image_resize |

Os builds do FFmpeg são licenciados pela GPL. Ao redistribuir os pacotes, mantenha a pasta de licenças junto
e indique onde obter o código-fonte do FFmpeg (link acima).

Os modelos de IA (Whisper e GGUF) não vêm no pacote; o usuário os baixa pelo app, e cada um segue a licença do seu autor.

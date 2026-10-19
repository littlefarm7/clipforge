# Como contribuir

1. Faça um fork e crie uma branch para sua mudança.
2. Rode localmente:
   ```bash
   cargo run --release
   ```
   O app usa `ffmpeg`, `ffprobe` e `yt-dlp` do PATH quando não estão empacotados.
3. Antes de abrir o pull request:
   ```bash
   cargo fmt && cargo clippy --release
   ```
4. Descreva no pull request o que mudou e como testou.

Para gerar os pacotes das 3 plataformas, veja `packaging/build-all.sh`.
Relatos de bugs e ideias são bem-vindos em "Issues".

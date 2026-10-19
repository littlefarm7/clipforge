# ClipForge 0.1.0 — AppImage (Linux x86_64)

O GitHub não aceita arquivos com mais de 100 MB no repositório, então o AppImage foi dividido em partes.

## Como montar e rodar

1. Baixe todas as partes (`.part00`, `.part01`, `.part02`) e o arquivo `.sha256` para a mesma pasta.
2. No terminal, dentro dessa pasta, junte as partes:
   ```bash
   cat ClipForge-0.1.0-x86_64.AppImage.part* > ClipForge-0.1.0-x86_64.AppImage
   ```
3. Confira se o arquivo está correto (deve mostrar "OK"):
   ```bash
   sha256sum -c ClipForge-0.1.0-x86_64.AppImage.sha256
   ```
4. Dê permissão e abra:
   ```bash
   chmod +x ClipForge-0.1.0-x86_64.AppImage
   ./ClipForge-0.1.0-x86_64.AppImage
   ```

Atalho (baixa, junta e confere):
```bash
git clone --depth 1 -b appimage https://github.com/littlefarm7/clipforge clipforge-appimage
cd clipforge-appimage
cat ClipForge-0.1.0-x86_64.AppImage.part* > ClipForge-0.1.0-x86_64.AppImage
sha256sum -c ClipForge-0.1.0-x86_64.AppImage.sha256 && chmod +x ClipForge-0.1.0-x86_64.AppImage
```

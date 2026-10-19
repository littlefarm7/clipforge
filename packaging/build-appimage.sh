#!/bin/bash
# Gera o AppImage do ClipForge. Requer: rust, cmake, python3, pip install ziglang, cargo install cargo-zigbuild.
# Os binários são ligados à glibc 2.28 para rodar em LMDE 5/6/7, Debian 10+, Ubuntu 20.04+ e similares.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${WORK:-$HOME/clipforge-build}"
mkdir -p "$WORK" && cd "$WORK"
export PATH="$PATH:$(python3 -c 'import ziglang,os;print(os.path.dirname(ziglang.__file__))')"

# 1. Ferramentas nativas (whisper.cpp e llama.cpp, variantes AVX2 e genérica)
[ -d whisper.cpp ] || git clone --depth 1 https://github.com/ggml-org/whisper.cpp
[ -d llama.cpp ] || git clone --depth 1 https://github.com/ggml-org/llama.cpp
mkdir -p zig
printf '#!/bin/sh\nexec python3 -m ziglang cc -target x86_64-linux-gnu.2.28 "$@"\n' > zig/zcc
printf '#!/bin/sh\nexec python3 -m ziglang c++ -target x86_64-linux-gnu.2.28 "$@"\n' > zig/zcxx
chmod +x zig/zcc zig/zcxx
sed "s|~/build|$WORK|g" "$ROOT/packaging/build-native-tools.sh" > build-native.sh && bash build-native.sh

# 2. ffmpeg estático, yt-dlp e deno
[ -f ff.tar.xz ] || curl -L -o ff.tar.xz https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz
[ -d ffx ] || { mkdir ffx && tar -xJf ff.tar.xz -C ffx; }
[ -f yt-dlp_linux ] || curl -L -o yt-dlp_linux https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux
[ -f deno ] || { curl -L -o deno.zip https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip && python3 -m zipfile -e deno.zip . ; }
[ -f appimagetool ] || curl -L -o appimagetool https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
[ -f runtime-x86_64 ] || curl -L -o runtime-x86_64 https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64
chmod +x appimagetool deno

# 3. Aplicativo principal
(cd "$ROOT" && cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28)

# 4. AppDir
A="$WORK/AppDir"; rm -rf "$A"
mkdir -p "$A/usr/bin" "$A/usr/share/applications" "$A/usr/share/icons/hicolor/scalable/apps" "$A/usr/share/doc/clipforge"
P="$ROOT/packaging"
cp "$P/AppRun" "$P/clipforge.desktop" "$P/clipforge.svg" "$A/"
cp "$P/clipforge.desktop" "$A/usr/share/applications/"
cp "$P/clipforge.svg" "$A/usr/share/icons/hicolor/scalable/apps/"
ln -s clipforge.svg "$A/.DirIcon"
cp "$ROOT/target/x86_64-unknown-linux-gnu/release/clipforge" "$A/usr/bin/"
FFD=$(ls -d ffx/*/); cp "$FFD/bin/ffmpeg" "$FFD/bin/ffprobe" "$A/usr/bin/"; cp "$FFD/LICENSE.txt" "$A/usr/share/doc/clipforge/LICENSE-ffmpeg.txt"
cp yt-dlp_linux "$A/usr/bin/yt-dlp"; cp deno "$A/usr/bin/deno"
for v in avx2 generic; do
  strip -o "$A/usr/bin/llama-server-$v" "llama.cpp/build-$v/bin/llama-server"
  strip -o "$A/usr/bin/whisper-cli-$v" "whisper.cpp/build-$v/bin/whisper-cli"
done
cp whisper.cpp/LICENSE "$A/usr/share/doc/clipforge/LICENSE-whisper.cpp.txt"
cp llama.cpp/LICENSE "$A/usr/share/doc/clipforge/LICENSE-llama.cpp.txt"
cp "$ROOT"/assets/fonts/OFL-*.txt "$A/usr/share/doc/clipforge/"
chmod +x "$A"/usr/bin/*

# 5. AppImage
VER=$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)
mkdir -p "$ROOT/../dist"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 ./appimagetool --runtime-file runtime-x86_64 --comp zstd "$A" "$ROOT/../dist/ClipForge-$VER-x86_64.AppImage"
echo "Pronto: $ROOT/../dist/ClipForge-$VER-x86_64.AppImage"

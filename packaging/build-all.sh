#!/bin/bash
# Gera os pacotes do ClipForge para Linux (AppImage), Windows (zip) e macOS (zip com .app),
# tudo a partir de um Linux x86_64.
#
# Requisitos: rust (rustup), cmake, git, curl, zip, python3, strip
#   pip install ziglang==0.13.0
#   cargo install cargo-zigbuild
#   cargo install apple-codesign --bin rcodesign   (opcional: assina os binários do macOS)
#
# Uso: ./packaging/build-all.sh [linux] [windows] [macos]   (sem argumentos = todos)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
P="$ROOT/packaging"
WORK="${WORK:-$HOME/clipforge-build}"
DIST="${DIST:-$ROOT/dist}"
VER=$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)
LLAMA_TAG="${LLAMA_TAG:-b11723}"
TARGETS="${*:-linux windows macos}"
GH=https://github.com
JOBS=$(nproc)

mkdir -p "$WORK" "$DIST"
cd "$WORK"
export PATH="$PATH:$(python3 -c 'import ziglang,os;print(os.path.dirname(ziglang.__file__))')"

want() { [[ " $TARGETS " == *" $1 "* ]]; }
fetch() { [ -s "$2" ] || curl -fL --retry 3 -o "$2" "$1"; }

# ---------- Compiladores zig (C/C++ para qualquer sistema) ----------
mkdir -p zig
for t in x86_64-linux-gnu.2.28 x86_64-windows-gnu aarch64-macos x86_64-macos; do
  printf '#!/bin/sh\nexec python3 -m ziglang cc -target %s "$@"\n' "$t" > "zig/cc-$t"
  printf '#!/bin/sh\nexec python3 -m ziglang c++ -target %s "$@"\n' "$t" > "zig/cxx-$t"
done
printf '#!/bin/sh\nexec python3 -m ziglang ar "$@"\n' > zig/ar
printf '#!/bin/sh\nexec python3 -m ziglang ranlib "$@"\n' > zig/ranlib
chmod +x zig/*

# cbuild <fonte> <nome> <alvo-zig> <sistema> <cpu> <alvo-cmake> [flags...]
cbuild() {
  local src=$1 name=$2 t=$3 sys=$4 cpu=$5 tgt=$6; shift 6
  [ -f "$src/build-$name/bin/$tgt" ] || [ -f "$src/build-$name/bin/$tgt.exe" ] && return 0
  echo ">> $src ($name)"
  cmake -S "$src" -B "$src/build-$name" -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_SYSTEM_NAME="$sys" -DCMAKE_SYSTEM_PROCESSOR="$cpu" \
    -DCMAKE_C_COMPILER="$WORK/zig/cc-$t" -DCMAKE_CXX_COMPILER="$WORK/zig/cxx-$t" \
    -DCMAKE_AR="$WORK/zig/ar" -DCMAKE_RANLIB="$WORK/zig/ranlib" -DCMAKE_INSTALL_NAME_TOOL=/bin/true \
    -DCMAKE_OSX_DEPLOYMENT_TARGET=11.0 \
    -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_OPENMP=OFF -DGGML_BACKEND_DL=OFF \
    -DGGML_METAL=OFF -DGGML_BLAS=OFF -DGGML_ACCELERATE=OFF \
    -DWHISPER_BUILD_TESTS=OFF -DWHISPER_SDL2=OFF -DWHISPER_CURL=OFF \
    -DLLAMA_CURL=OFF -DLLAMA_OPENSSL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF \
    "$@" > "$WORK/cmake-$name.log"
  cmake --build "$src/build-$name" -j"$JOBS" --target "$tgt" >> "$WORK/cmake-$name.log"
}
AVX2="-DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON"
NOAVX="-DGGML_AVX=OFF -DGGML_AVX2=OFF -DGGML_FMA=OFF -DGGML_F16C=OFF"

[ -d whisper.cpp ] || git clone --depth 1 $GH/ggml-org/whisper.cpp
[ -d llama.cpp ] || git clone --depth 1 $GH/ggml-org/llama.cpp

# ---------- Aplicativo principal ----------
build_app() {
  (cd "$ROOT" && cargo zigbuild --release --target "$1")
}

copy_licenses() { # destino
  mkdir -p "$1"
  cp "$ROOT/LICENSE" "$1/LICENSE-ClipForge.txt"
  cp whisper.cpp/LICENSE "$1/LICENSE-whisper.cpp.txt"
  cp llama.cpp/LICENSE "$1/LICENSE-llama.cpp.txt"
  cp "$ROOT"/assets/fonts/OFL-*.txt "$1/"
  cp "$ROOT/THIRD_PARTY.md" "$1/"
}

# ================= Linux (AppImage) =================
if want linux; then
  echo "=== Linux"
  for v in avx2 generic; do
    F=$AVX2; [ $v = generic ] && F=$NOAVX
    cbuild whisper.cpp linux-$v x86_64-linux-gnu.2.28 Linux x86_64 whisper-cli $F
    cbuild llama.cpp linux-$v x86_64-linux-gnu.2.28 Linux x86_64 llama-server -DLLAMA_BUILD_SERVER=ON -DLLAMA_BUILD_TOOLS=ON $F
  done
  fetch $GH/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz ff-linux.tar.xz
  [ -d ff-linux ] || { mkdir ff-linux && tar -xJf ff-linux.tar.xz -C ff-linux --strip-components=1; }
  fetch $GH/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux yt-dlp_linux
  fetch $GH/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip deno-linux.zip
  fetch $GH/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage appimagetool
  fetch $GH/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64 runtime-x86_64
  chmod +x appimagetool
  build_app x86_64-unknown-linux-gnu.2.28

  A="$WORK/AppDir"; rm -rf "$A"
  mkdir -p "$A/usr/bin" "$A/usr/share/applications" "$A/usr/share/icons/hicolor/scalable/apps"
  cp "$P/AppRun" "$P/clipforge.desktop" "$P/clipforge.svg" "$A/"
  cp "$P/clipforge.desktop" "$A/usr/share/applications/"
  cp "$P/clipforge.svg" "$A/usr/share/icons/hicolor/scalable/apps/"
  ln -s clipforge.svg "$A/.DirIcon"
  cp "$ROOT/target/x86_64-unknown-linux-gnu/release/clipforge" "$A/usr/bin/"
  cp ff-linux/bin/ffmpeg ff-linux/bin/ffprobe "$A/usr/bin/"
  cp yt-dlp_linux "$A/usr/bin/yt-dlp"
  unzip -o -q deno-linux.zip -d "$A/usr/bin/"
  for v in avx2 generic; do
    strip -o "$A/usr/bin/whisper-cli-$v" "whisper.cpp/build-linux-$v/bin/whisper-cli"
    strip -o "$A/usr/bin/llama-server-$v" "llama.cpp/build-linux-$v/bin/llama-server"
  done
  copy_licenses "$A/usr/share/doc/clipforge"
  cp ff-linux/LICENSE.txt "$A/usr/share/doc/clipforge/LICENSE-ffmpeg.txt"
  chmod +x "$A"/usr/bin/*
  rm -f "$DIST/ClipForge-$VER-x86_64.AppImage"
  ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 ./appimagetool --runtime-file runtime-x86_64 --comp zstd "$A" "$DIST/ClipForge-$VER-x86_64.AppImage"
fi

# ================= Windows (zip portátil) =================
if want windows; then
  echo "=== Windows"
  rustup target add x86_64-pc-windows-gnu >/dev/null
  for v in avx2 generic; do
    F=$AVX2; [ $v = generic ] && F=$NOAVX
    cbuild whisper.cpp win-$v x86_64-windows-gnu Windows AMD64 whisper-cli $F
  done
  fetch $GH/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip ff-win.zip
  [ -d ff-win ] || { mkdir ff-win && unzip -q ff-win.zip -d ff-win && mv ff-win/*/* ff-win/; }
  fetch $GH/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe yt-dlp.exe
  fetch $GH/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip deno-win.zip
  fetch $GH/ggml-org/llama.cpp/releases/download/$LLAMA_TAG/llama-$LLAMA_TAG-bin-win-cpu-x64.zip llama-win.zip
  build_app x86_64-pc-windows-gnu

  W="$WORK/win/ClipForge"; rm -rf "$WORK/win"; mkdir -p "$W/llama"
  cp "$ROOT/target/x86_64-pc-windows-gnu/release/clipforge.exe" "$W/ClipForge.exe"
  cp ff-win/bin/ffmpeg.exe ff-win/bin/ffprobe.exe yt-dlp.exe "$W/"
  unzip -o -q deno-win.zip -d "$W/"
  for v in avx2 generic; do cp "whisper.cpp/build-win-$v/bin/whisper-cli.exe" "$W/whisper-cli-$v.exe"; done
  # llama.cpp oficial (escolhe sozinho a melhor versão para a CPU); só o servidor e as DLLs
  unzip -q llama-win.zip -d "$WORK/win/llama-all"
  cp "$WORK/win/llama-all/"*.dll "$WORK/win/llama-all/llama-server.exe" "$W/llama/"
  cp "$P/clipforge.ico" "$W/"
  cp "$P/LEIA-ME-windows.txt" "$W/LEIA-ME.txt"
  copy_licenses "$W/licencas"
  cp ff-win/LICENSE.txt "$W/licencas/LICENSE-ffmpeg.txt"
  cp "$WORK/win/llama-all/LICENSE-"* "$W/licencas/" 2>/dev/null || true
  rm -f "$DIST/ClipForge-$VER-windows-x64.zip"
  (cd "$WORK/win" && zip -q -r -9 "$DIST/ClipForge-$VER-windows-x64.zip" ClipForge)
fi

# ================= macOS (.app em zip) =================
if want macos; then
  echo "=== macOS"
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cbuild whisper.cpp mac-arm64 aarch64-macos Darwin arm64 whisper-cli
  cbuild whisper.cpp mac-x64 x86_64-macos Darwin x86_64 whisper-cli $AVX2
  fetch $GH/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_macos yt-dlp_macos
  for arch in arm64 x64; do
    case $arch in arm64) RT=aarch64-apple-darwin; DN=aarch64-apple-darwin;; x64) RT=x86_64-apple-darwin; DN=x86_64-apple-darwin;; esac
    for b in ffmpeg ffprobe; do
      fetch $GH/eugeneware/ffmpeg-static/releases/latest/download/$b-darwin-$arch.gz $b-darwin-$arch.gz
      [ -s $b-darwin-$arch ] || gunzip -c $b-darwin-$arch.gz > $b-darwin-$arch
    done
    fetch $GH/denoland/deno/releases/latest/download/deno-$DN.zip deno-mac-$arch.zip
    fetch $GH/ggml-org/llama.cpp/releases/download/$LLAMA_TAG/llama-$LLAMA_TAG-bin-macos-$arch.tar.gz llama-mac-$arch.tar.gz
    build_app $RT

    M="$WORK/mac-$arch"; rm -rf "$M"; APP="$M/ClipForge.app/Contents"
    mkdir -p "$APP/MacOS/llama" "$APP/Resources"
    sed "s/@VERSION@/$VER/g" "$P/Info.plist" > "$APP/Info.plist"
    cp "$P/clipforge.icns" "$APP/Resources/"
    cp "$ROOT/target/$RT/release/clipforge" "$APP/MacOS/ClipForge"
    cp ffmpeg-darwin-$arch "$APP/MacOS/ffmpeg"; cp ffprobe-darwin-$arch "$APP/MacOS/ffprobe"
    cp yt-dlp_macos "$APP/MacOS/yt-dlp"
    unzip -o -q deno-mac-$arch.zip -d "$APP/MacOS/"
    cp "whisper.cpp/build-mac-$arch/bin/whisper-cli" "$APP/MacOS/whisper-cli"
    # llama.cpp oficial (com Metal no Apple Silicon); só o servidor e as bibliotecas
    mkdir -p "$M/llama-all" && tar -xzf llama-mac-$arch.tar.gz -C "$M/llama-all" --strip-components=1
    cp -P "$M/llama-all/"*.dylib "$M/llama-all/llama-server" "$APP/MacOS/llama/"
    copy_licenses "$APP/Resources/licencas"
    chmod +x "$APP/MacOS/"* "$APP/MacOS/llama/llama-server"
    # Apple Silicon só executa binários assinados: assinatura ad-hoc (sem certificado)
    if command -v rcodesign >/dev/null; then
      for f in "$APP/MacOS/"* "$APP/MacOS/llama/"*; do
        [ -f "$f" ] && [ ! -L "$f" ] && rcodesign sign "$f" >/dev/null 2>&1 || true
      done
      rcodesign sign "$M/ClipForge.app" >/dev/null 2>&1 || true
    fi
    cp "$P/LEIA-ME-macos.txt" "$M/LEIA-ME.txt"
    rm -f "$DIST/ClipForge-$VER-macos-$arch.zip"
    (cd "$M" && zip -q -r -y -9 "$DIST/ClipForge-$VER-macos-$arch.zip" ClipForge.app LEIA-ME.txt)
  done
fi

(cd "$DIST" && sha256sum ClipForge-$VER-* 2>/dev/null | grep -v sha256 > SHA256SUMS.txt || true)
echo "Pronto. Pacotes em $DIST:"
ls -la "$DIST"

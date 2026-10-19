#!/bin/bash
set -e
Z=~/build/zig
for variant in avx2 generic; do
  if [ $variant = avx2 ]; then F="-DGGML_AVX=ON -DGGML_AVX2=ON -DGGML_FMA=ON -DGGML_F16C=ON -DGGML_BMI2=ON"; else F="-DGGML_AVX=OFF -DGGML_AVX2=OFF -DGGML_FMA=OFF -DGGML_F16C=OFF"; fi
  COMMON="-DCMAKE_BUILD_TYPE=Release -DCMAKE_C_COMPILER=$Z/zcc -DCMAKE_CXX_COMPILER=$Z/zcxx -DBUILD_SHARED_LIBS=OFF -DGGML_NATIVE=OFF -DGGML_OPENMP=OFF -DGGML_BACKEND_DL=OFF $F"
  cd ~/build/whisper.cpp
  cmake -B build-$variant $COMMON -DWHISPER_BUILD_TESTS=OFF -DWHISPER_SDL2=OFF -DWHISPER_CURL=OFF
  cmake --build build-$variant -j4 --target whisper-cli
  cd ~/build/llama.cpp
  cmake -B build-$variant $COMMON -DLLAMA_CURL=OFF -DLLAMA_OPENSSL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_TOOLS=ON -DLLAMA_BUILD_SERVER=ON
  cmake --build build-$variant -j4 --target llama-server
done
echo DONE

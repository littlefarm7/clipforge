//! Catálogo de modelos locais (Whisper e LLM GGUF) e gerenciador de downloads.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub size_mb: u32,
    pub desc: &'static str,
}

pub fn whisper_catalog() -> Vec<ModelInfo> {
    macro_rules! w {
        ($id:literal, $name:literal, $size:literal, $desc:literal) => {
            ModelInfo {
                id: $id,
                name: $name,
                file: concat!("ggml-", $id, ".bin"),
                url: concat!(
                    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-",
                    $id,
                    ".bin"
                ),
                size_mb: $size,
                desc: $desc,
            }
        };
    }
    vec![
        w!("tiny", "Whisper Tiny", 75, "Muito rápido, precisão baixa."),
        w!(
            "base",
            "Whisper Base",
            142,
            "Rápido. Bom para testar (recomendado para PCs simples)."
        ),
        w!(
            "small",
            "Whisper Small",
            466,
            "Bom equilíbrio entre velocidade e precisão."
        ),
        w!(
            "large-v3-turbo-q5_0",
            "Whisper Large v3 Turbo (Q5)",
            547,
            "Ótima precisão em português. Mais lento."
        ),
        w!(
            "medium",
            "Whisper Medium",
            1500,
            "Alta precisão, lento em CPU."
        ),
    ]
}

pub fn llm_catalog() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "qwen2.5-1.5b",
            name: "Qwen 2.5 1.5B Instruct (Q4_K_M)",
            file: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            url: "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            size_mb: 1120,
            desc: "Leve (≈2 GB de RAM). Funciona em quase qualquer PC.",
        },
        ModelInfo {
            id: "qwen2.5-3b",
            name: "Qwen 2.5 3B Instruct (Q4_K_M)",
            file: "qwen2.5-3b-instruct-q4_k_m.gguf",
            url: "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
            size_mb: 2100,
            desc: "Recomendado (≈4 GB de RAM). Boa escolha de trechos.",
        },
        ModelInfo {
            id: "llama3.2-3b",
            name: "Llama 3.2 3B Instruct (Q4_K_M)",
            file: "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            url: "https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            size_mb: 2020,
            desc: "Alternativa da Meta (≈4 GB de RAM).",
        },
        ModelInfo {
            id: "qwen2.5-7b",
            name: "Qwen 2.5 7B Instruct (Q4_K_M)",
            file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
            url: "https://huggingface.co/bartowski/Qwen2.5-7B-Instruct-GGUF/resolve/main/Qwen2.5-7B-Instruct-Q4_K_M.gguf",
            size_mb: 4680,
            desc: "Melhor qualidade (≈8 GB de RAM). Mais lento.",
        },
    ]
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DownloadState {
    pub kind: String,
    pub file: String,
    pub downloaded: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub fn dir_for(kind: &str) -> PathBuf {
    if kind == "whisper" {
        crate::paths::whisper_models_dir()
    } else {
        crate::paths::llm_models_dir()
    }
}

/// Lista arquivos instalados de um tipo (inclui modelos personalizados).
pub fn installed(kind: &str) -> Vec<(String, u64)> {
    let ext = if kind == "whisper" { "bin" } else { "gguf" };
    let mut v: Vec<(String, u64)> = std::fs::read_dir(dir_for(kind))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let md = e.metadata().ok()?;
            (md.is_file() && name.ends_with(ext)).then_some((name, md.len()))
        })
        .collect();
    v.sort();
    v
}

pub fn safe_file_name(name: &str) -> Result<String> {
    let n = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .split('?')
        .next()
        .unwrap_or("")
        .trim();
    if n.is_empty() || n.starts_with('.') || n.contains('\\') || n.contains("..") {
        bail!("nome de arquivo inválido");
    }
    Ok(n.to_string())
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("ClipForge/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("cliente http")
}

/// Baixa `url` para `dest`, chamando `progress(baixado, total)`.
pub async fn download(url: &str, dest: &Path, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let resp = http_client()
        .get(url)
        .send()
        .await
        .with_context(|| format!("falha ao conectar em {url}"))?;
    if !resp.status().is_success() {
        bail!("download falhou: HTTP {}", resp.status());
    }
    let total = resp.content_length().unwrap_or(0);
    let tmp = dest.with_extension("part");
    let mut f = tokio::fs::File::create(&tmp).await?;
    let mut got = 0u64;
    let mut stream = resp.bytes_stream();
    let mut last = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("conexão interrompida durante o download")?;
        f.write_all(&chunk).await?;
        got += chunk.len() as u64;
        if last.elapsed().as_millis() > 300 {
            progress(got, total);
            last = std::time::Instant::now();
        }
    }
    f.flush().await?;
    drop(f);
    if total > 0 && got < total {
        let _ = tokio::fs::remove_file(&tmp).await;
        bail!("download incompleto");
    }
    tokio::fs::rename(&tmp, dest).await?;
    progress(got, got);
    Ok(())
}

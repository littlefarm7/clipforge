//! Catálogo de modelos locais (Whisper e LLM GGUF) e gerenciador de downloads.

use crate::state::App;
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

/// Nível de qualidade do modo "No meu computador": um par de modelos (transcrição + IA).
#[derive(Debug, Clone, Serialize)]
pub struct Tier {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub whisper_id: &'static str,
    pub whisper_file: &'static str,
    pub llm_id: &'static str,
    pub llm_file: &'static str,
    /// Tamanho total para baixar
    pub size_mb: u32,
    /// Memória RAM recomendada
    pub min_ram_gb: f64,
}

pub fn tiers() -> Vec<Tier> {
    vec![
        Tier {
            id: "fast",
            name: "Leve",
            desc: "Funciona em qualquer computador. Cortes bons, mais rápido.",
            whisper_id: "base",
            whisper_file: "ggml-base.bin",
            llm_id: "qwen2.5-1.5b",
            llm_file: "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            size_mb: 1262,
            min_ram_gb: 4.0,
        },
        Tier {
            id: "balanced",
            name: "Equilibrado",
            desc: "Ótima escolha de cortes e legendas precisas.",
            whisper_id: "small",
            whisper_file: "ggml-small.bin",
            llm_id: "qwen2.5-3b",
            llm_file: "qwen2.5-3b-instruct-q4_k_m.gguf",
            size_mb: 2566,
            min_ram_gb: 8.0,
        },
        Tier {
            id: "best",
            name: "Máxima qualidade",
            desc: "Os melhores cortes. Precisa de um computador potente.",
            whisper_id: "large-v3-turbo-q5_0",
            whisper_file: "ggml-large-v3-turbo-q5_0.bin",
            llm_id: "qwen2.5-7b",
            llm_file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
            size_mb: 5227,
            min_ram_gb: 16.0,
        },
    ]
}

/// Nível indicado para a memória deste computador.
pub fn recommended_tier(ram_gb: f64) -> &'static str {
    // RAM desconhecida: escolhe o meio-termo
    if ram_gb <= 0.0 {
        "balanced"
    } else if ram_gb >= 15.0 {
        "best"
    } else if ram_gb >= 7.5 {
        "balanced"
    } else {
        "fast"
    }
}

/// Procura no catálogo o modelo pelo nome do arquivo.
pub fn by_file(kind: &str, file: &str) -> Option<ModelInfo> {
    let cat = if kind == "whisper" {
        whisper_catalog()
    } else {
        llm_catalog()
    };
    cat.into_iter().find(|m| m.file == file)
}

/// Começa a baixar um modelo em segundo plano (não faz nada se já estiver baixando).
pub fn start_download(app: &App, kind: &str, url: String, file: String) {
    let key = format!("{kind}/{file}");
    {
        let mut d = app.downloads.lock().unwrap();
        if d.get(&key).is_some_and(|s| !s.done) {
            return;
        }
        d.insert(
            key.clone(),
            DownloadState {
                kind: kind.into(),
                file: file.clone(),
                ..Default::default()
            },
        );
    }
    let dest = dir_for(kind).join(&file);
    let app2 = app.clone();
    tokio::spawn(async move {
        let k = key.clone();
        let res = download(&url, &dest, |got, total| {
            if let Some(s) = app2.downloads.lock().unwrap().get_mut(&k) {
                s.downloaded = got;
                s.total = total;
            }
        })
        .await;
        let mut d = app2.downloads.lock().unwrap();
        if let Some(s) = d.get_mut(&key) {
            s.done = true;
            if let Err(e) = res {
                s.error = Some(format!("{e:#}"));
            }
        }
    });
}

/// Garante que o modelo está no computador: se faltar, baixa e espera, informando o progresso.
pub async fn ensure(
    app: &App,
    kind: &str,
    file: &str,
    cancel: &tokio_util::sync::CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<()> {
    if dir_for(kind).join(file).is_file() {
        return Ok(());
    }
    let m =
        by_file(kind, file).with_context(|| format!("o arquivo {file} não está no computador"))?;
    start_download(app, kind, m.url.to_string(), m.file.to_string());
    let key = format!("{kind}/{file}");
    loop {
        if cancel.is_cancelled() {
            bail!("Cancelado");
        }
        let st = app.downloads.lock().unwrap().get(&key).cloned();
        match st {
            Some(s) if s.done => {
                if let Some(e) = s.error {
                    bail!("falha ao baixar {file}: {e}");
                }
                return Ok(());
            }
            Some(s) if s.total > 0 => progress(s.downloaded as f32 / s.total as f32),
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
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

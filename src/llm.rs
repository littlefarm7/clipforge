//! Cliente de chat compatível com OpenAI e servidor llama.cpp local.

use crate::config::Config;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::time::Duration;
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

pub struct Llm {
    base: String,
    key: String,
    model: String,
    client: reqwest::Client,
    /// Mantém o llama-server vivo enquanto o cliente existir.
    _server: Option<Child>,
    pub is_local: bool,
}

impl Llm {
    pub async fn from_config(cfg: &Config, cancel: &CancellationToken) -> Result<Option<Llm>> {
        match cfg.llm_provider.as_str() {
            "api" => {
                if cfg.llm_api_base.trim().is_empty() || cfg.llm_api_model.trim().is_empty() {
                    bail!("Configure o endereço e o modelo da API de IA.");
                }
                Ok(Some(Llm {
                    base: cfg.llm_api_base.trim().trim_end_matches('/').to_string(),
                    key: cfg.llm_api_key.trim().to_string(),
                    model: cfg.llm_api_model.trim().to_string(),
                    client: crate::models::http_client(),
                    _server: None,
                    is_local: false,
                }))
            }
            "local" => Ok(Some(start_local(cfg, cancel).await?)),
            _ => Ok(None),
        }
    }

    pub async fn chat(&self, system: &str, user: &str, want_json: bool) -> Result<String> {
        let url = format!("{}/chat/completions", self.base);
        let mut body = json!({
            "model": self.model,
            "temperature": 0.3,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        });
        if want_json {
            body["response_format"] = json!({"type": "json_object"});
        }
        let mut tries = 0;
        loop {
            tries += 1;
            let mut req = self
                .client
                .post(&url)
                .json(&body)
                .timeout(Duration::from_secs(1200));
            if !self.key.is_empty() {
                req = req.bearer_auth(&self.key);
            }
            let resp = req
                .send()
                .await
                .with_context(|| format!("falha ao conectar em {url}"))?;
            let st = resp.status();
            if st.is_success() {
                let v: serde_json::Value = resp.json().await?;
                let content = v["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                return Ok(content);
            }
            let text = resp.text().await.unwrap_or_default();
            // Alguns provedores não aceitam response_format: tenta de novo sem ele.
            if st.as_u16() == 400 && body.get("response_format").is_some() {
                body.as_object_mut().unwrap().remove("response_format");
                continue;
            }
            if (st.as_u16() == 429 || st.is_server_error()) && tries < 4 {
                tokio::time::sleep(Duration::from_secs(8 * tries)).await;
                continue;
            }
            bail!(
                "API de IA respondeu {st}: {}",
                text.chars().take(600).collect::<String>()
            );
        }
    }
}

async fn free_port() -> Result<u16> {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    Ok(l.local_addr()?.port())
}

async fn start_local(cfg: &Config, cancel: &CancellationToken) -> Result<Llm> {
    if cfg.llm_local_model.is_empty() {
        bail!(
            "Escolha um modelo de IA local em Configurações → IA (baixe um em Configurações → Modelos)."
        );
    }
    let model = paths::llm_models_dir().join(&cfg.llm_local_model);
    if !model.is_file() {
        bail!(
            "Modelo local \"{}\" não encontrado. Baixe-o em Configurações → Modelos.",
            cfg.llm_local_model
        );
    }
    let port = free_port().await?;
    let mut c = crate::proc::command(paths::llama_server());
    c.arg("-m").arg(&model);
    c.args(["--host", "127.0.0.1", "--port", &port.to_string()]);
    c.args([
        "-c",
        &cfg.llm_ctx.max(2048).to_string(),
        "-t",
        &cfg.threads().to_string(),
        "-np",
        "1",
    ]);
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::null());
    c.kill_on_drop(true);
    let mut child = c
        .spawn()
        .context("não foi possível iniciar o llama-server")?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    let health = format!("http://127.0.0.1:{port}/health");
    let started = std::time::Instant::now();
    loop {
        if cancel.is_cancelled() {
            bail!("Cancelado");
        }
        if let Some(st) = child.try_wait()? {
            bail!(
                "o llama-server encerrou ({st}). Verifique se há memória RAM suficiente para o modelo."
            );
        }
        if let Ok(r) = client.get(&health).send().await
            && r.status().is_success()
        {
            break;
        }
        if started.elapsed() > Duration::from_secs(300) {
            bail!("o modelo local demorou demais para carregar");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(Llm {
        base: format!("http://127.0.0.1:{port}/v1"),
        key: String::new(),
        model: "local".into(),
        client,
        _server: Some(child),
        is_local: true,
    })
}

/// Extrai o primeiro objeto JSON de um texto (o modelo às vezes adiciona comentários).
pub fn extract_json(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    let start = s.find(['{', '['])?;
    let end = s.rfind(['}', ']'])?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&s[start..=end]).ok()
}

//! Cliente de chat compatível com OpenAI e servidor llama.cpp local.

use crate::config::Config;
use crate::paths;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::time::Duration;
use tokio::process::Child;
use tokio_util::sync::CancellationToken;

/// O texto enviado não cabe no contexto do modelo (ou no limite de tokens do provedor).
#[derive(Debug)]
pub struct ContextOverflow;

/// Tokens reservados para a resposta do modelo local (lista de clipes em JSON).
/// Para APIs não há limite fixo: modelos que "pensam" antes de responder precisam de folga.
const LOCAL_RESPONSE_TOKENS: u32 = 2048;

impl std::fmt::Display for ContextOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("o trecho da transcrição é maior que o contexto do modelo de IA")
    }
}

impl std::error::Error for ContextOverflow {}

fn is_context_overflow(body: &str) -> bool {
    let b = body.to_lowercase();
    b.contains("exceed_context_size")
        || b.contains("context_length_exceeded")
        || b.contains("request too large")
        || b.contains("reduce the length")
        || (b.contains("context")
            && (b.contains("exceed") || b.contains("too long") || b.contains("maximum")))
}

pub struct Llm {
    base: String,
    key: String,
    model: String,
    client: reqwest::Client,
    /// Mantém o llama-server vivo enquanto o cliente existir.
    _server: Option<Child>,
    pub is_local: bool,
    /// Tamanho real do contexto do modelo local (tokens)
    pub ctx: u32,
    /// Endereço raiz do llama-server local (para /tokenize)
    root: Option<String>,
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
                    ctx: 0,
                    root: None,
                }))
            }
            "local" => Ok(Some(start_local(cfg, cancel).await?)),
            _ => Ok(None),
        }
    }

    /// Quantos tokens de transcrição cabem em um pedido.
    pub fn input_budget(&self) -> usize {
        if self.is_local {
            // Reserva espaço para as instruções (~700 tokens) e para a resposta
            (self.ctx as usize)
                .saturating_sub(LOCAL_RESPONSE_TOKENS as usize + 700)
                .max(600)
        } else {
            // Planos gratuitos (ex.: Groq) limitam os tokens por minuto; blocos menores evitam recusas
            6000
        }
    }

    /// Maior pedido (instruções + transcrição) aceito pelo modelo local.
    pub fn prompt_limit(&self) -> usize {
        (self.ctx as usize).saturating_sub(LOCAL_RESPONSE_TOKENS as usize + 32)
    }

    /// Conta os tokens com o tokenizador do próprio modelo (só no modelo local).
    pub async fn count_tokens(&self, text: &str) -> Option<usize> {
        let root = self.root.as_ref()?;
        let r = self
            .client
            .post(format!("{root}/tokenize"))
            .json(&json!({ "content": text }))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .ok()?;
        let v: serde_json::Value = r.json().await.ok()?;
        Some(v["tokens"].as_array()?.len())
    }

    /// Quantos caracteres cabem, em média, em um token deste modelo para este texto.
    /// No modelo local é medido; nas APIs usa um valor conservador.
    pub async fn chars_per_token(&self, sample: &str) -> f64 {
        let chars = sample.chars().count();
        if chars > 200
            && let Some(n) = self.count_tokens(sample).await
            && n > 0
        {
            // 10% de margem de segurança
            return (chars as f64 / n as f64 * 0.9).clamp(0.8, 5.0);
        }
        2.5
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
        if self.is_local {
            body["max_tokens"] = json!(LOCAL_RESPONSE_TOKENS);
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
            if is_context_overflow(&text) {
                return Err(ContextOverflow.into());
            }
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
    // O llama-server pode reduzir o contexto para caber na memória: usa o valor real
    let root = format!("http://127.0.0.1:{port}");
    let mut ctx = cfg.llm_ctx.max(2048);
    if let Ok(r) = client.get(format!("{root}/props")).send().await
        && let Ok(v) = r.json::<serde_json::Value>().await
        && let Some(n) = v["default_generation_settings"]["n_ctx"]
            .as_u64()
            .or_else(|| v["n_ctx"].as_u64())
        && n > 0
    {
        ctx = n as u32;
    }
    Ok(Llm {
        base: format!("{root}/v1"),
        key: String::new(),
        model: "local".into(),
        client,
        _server: Some(child),
        is_local: true,
        ctx,
        root: Some(root),
    })
}

/// Remove o raciocínio de modelos que "pensam" antes de responder (<think>...</think>).
fn strip_thinking(s: &str) -> &str {
    match (s.find("<think>"), s.rfind("</think>")) {
        (_, Some(end)) => &s[end + "</think>".len()..],
        // Raciocínio sem fim (resposta cortada): não há JSON aproveitável depois dele
        (Some(_), None) => "",
        _ => s,
    }
}

/// Posição do `}` que fecha o `{` em `open`, respeitando textos entre aspas.
fn matching_brace(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in s.iter().enumerate().skip(open) {
        if in_str {
            match c {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extrai o JSON da resposta do modelo. Aceita texto em volta, blocos ```json,
/// raciocínio <think> e respostas cortadas no meio (aproveita os clipes completos).
pub fn extract_json(s: &str) -> Option<serde_json::Value> {
    let s = strip_thinking(s).trim();
    if let Ok(v) = serde_json::from_str(s) {
        return Some(v);
    }
    if let (Some(start), Some(end)) = (s.find(['{', '[']), s.rfind(['}', ']']))
        && end > start
        && let Ok(v) = serde_json::from_str(&s[start..=end])
    {
        return Some(v);
    }
    // Resposta incompleta: recolhe cada objeto de clipe que esteja inteiro
    let bytes = s.as_bytes();
    let mut clips = Vec::new();
    let mut i = 0;
    while let Some(pos) = s[i..].find('{') {
        let open = i + pos;
        match matching_brace(bytes, open) {
            Some(close) => match serde_json::from_str::<serde_json::Value>(&s[open..=close]) {
                Ok(v) if v.get("start").is_some() => {
                    clips.push(v);
                    i = close + 1;
                }
                _ => i = open + 1,
            },
            None => i = open + 1,
        }
    }
    (!clips.is_empty()).then_some(serde_json::Value::Array(clips))
}

#[cfg(test)]
mod tests {
    use super::{extract_json, is_context_overflow};

    #[test]
    fn le_json_com_raciocinio_e_resposta_cortada() {
        let v = extract_json("<think>vou escolher {start} e {end}</think>\n```json\n{\"clips\":[{\"start\":1,\"end\":30}]}\n```").unwrap();
        assert_eq!(v["clips"][0]["end"], 30);
        // Cortada no meio do terceiro clipe: aproveita os dois primeiros
        let v = extract_json(r#"{"clips":[{"start":1,"end":30,"title":"a {b}"},{"start":40,"end":80,"title":"c"},{"start":90,"en"#).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert!(extract_json("<think>pensando sem fim {").is_none());
    }

    #[test]
    fn reconhece_erro_de_contexto() {
        // Erro real do llama-server
        assert!(is_context_overflow(
            r#"{"error":{"code":400,"message":"request (9901 tokens) exceeds the available context size (8192 tokens), try increasing it","type":"exceed_context_size_error","n_prompt_tokens":9901,"n_ctx":8192}}"#
        ));
        // OpenAI
        assert!(is_context_overflow(
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens","code":"context_length_exceeded"}}"#
        ));
        // Groq (limite de tokens por minuto)
        assert!(is_context_overflow(
            r#"{"error":{"message":"Request too large for model on tokens per minute (TPM): Limit 12000, Requested 15000"}}"#
        ));
        assert!(!is_context_overflow(
            r#"{"error":{"message":"Invalid API key"}}"#
        ));
    }
}

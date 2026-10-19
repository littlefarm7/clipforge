//! Transcrição com timestamps por palavra: whisper.cpp local ou API compatível com OpenAI.

use crate::config::Config;
use crate::paths;
use crate::proc;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Transcript {
    pub language: String,
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// Índices [first, last] em `words`
    pub first: usize,
    pub last: usize,
}

impl Transcript {
    /// Agrupa palavras em frases (por pontuação, pausas ou tamanho).
    pub fn segments(&self) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut first = 0usize;
        for i in 0..self.words.len() {
            let w = &self.words[i];
            let next_gap = self
                .words
                .get(i + 1)
                .map(|n| n.start - w.end)
                .unwrap_or(99.0);
            let punct = w.text.trim_end().ends_with(['.', '?', '!', '…']);
            let count = i - first + 1;
            if punct
                || next_gap > 0.9
                || count >= 30
                || (count >= 18 && w.text.trim_end().ends_with(','))
            {
                out.push(self.make_segment(first, i));
                first = i + 1;
            }
        }
        if first < self.words.len() {
            out.push(self.make_segment(first, self.words.len() - 1));
        }
        out
    }

    fn make_segment(&self, first: usize, last: usize) -> Segment {
        let text = self.words[first..=last]
            .iter()
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ");
        Segment {
            start: self.words[first].start,
            end: self.words[last].end,
            text,
            first,
            last,
        }
    }

    pub fn text_between(&self, start: f64, end: f64) -> String {
        self.words
            .iter()
            .filter(|w| w.end > start && w.start < end)
            .map(|w| w.text.trim())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn clean_word(t: &str) -> Option<String> {
    let s = t.trim();
    if s.is_empty()
        || (s.starts_with('[') && s.ends_with(']'))
        || (s.starts_with('(') && s.ends_with(')'))
    {
        return None;
    }
    Some(s.to_string())
}

pub async fn transcribe(
    cfg: &Config,
    src: &Path,
    work: &Path,
    cancel: &CancellationToken,
    progress: impl FnMut(f32, &str),
) -> Result<Transcript> {
    if cfg.stt_provider == "api" {
        transcribe_api(cfg, src, work, cancel, progress).await
    } else {
        transcribe_local(cfg, src, work, cancel, progress).await
    }
}

async fn transcribe_local(
    cfg: &Config,
    src: &Path,
    work: &Path,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32, &str),
) -> Result<Transcript> {
    let model = paths::whisper_models_dir().join(&cfg.whisper_model);
    if !model.is_file() {
        bail!(
            "Modelo Whisper \"{}\" não encontrado. Abra Configurações → Modelos e baixe um modelo, ou use transcrição por API.",
            cfg.whisper_model
        );
    }
    progress(0.0, "Extraindo áudio");
    let wav = work.join("audio16k.wav");
    let mut c = crate::proc::command(paths::ffmpeg());
    c.args(["-hide_banner", "-y", "-i"])
        .arg(src)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&wav);
    proc::run(c, cancel, |_| {})
        .await
        .context("falha ao extrair o áudio")?;

    progress(0.0, "Transcrevendo (whisper.cpp)");
    let out_base = work.join("whisper");
    let mut c = crate::proc::command(paths::whisper_cli());
    c.arg("-m")
        .arg(&model)
        .arg("-f")
        .arg(&wav)
        .arg("-of")
        .arg(&out_base);
    c.args([
        "-oj",
        "-ml",
        "1",
        "-sow",
        "-pp",
        "-t",
        &cfg.threads().to_string(),
        "-l",
        &cfg.language,
    ]);
    proc::run(c, cancel, |line| {
        if let Some(i) = line.find("progress =") {
            let n: String = line[i + 10..]
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            if let Ok(p) = n.parse::<f32>() {
                progress(p / 100.0, "Transcrevendo (whisper.cpp)");
            }
        }
    })
    .await?;
    let _ = tokio::fs::remove_file(&wav).await;

    let json_path = out_base.with_extension("json");
    let raw = tokio::fs::read(&json_path)
        .await
        .context("saída do whisper não encontrada")?;
    let v: serde_json::Value = serde_json::from_slice(&raw).context("JSON do whisper inválido")?;
    let language = v["result"]["language"].as_str().unwrap_or("").to_string();
    let mut words: Vec<Word> = Vec::new();
    for item in v["transcription"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let text = item["text"].as_str().unwrap_or("");
        let from = item["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let to = item["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0;
        // Com -ml 1 o whisper às vezes separa pontuação em um item próprio
        let t = text.trim();
        if !t.is_empty()
            && t.chars().all(|c| c.is_ascii_punctuation())
            && !t.starts_with('[')
            && let Some(last) = words.last_mut()
        {
            last.text.push_str(t);
            continue;
        }
        if let Some(t) = clean_word(text) {
            words.push(Word {
                start: from,
                end: to.max(from + 0.05),
                text: t,
            });
        }
    }
    fix_timings(&mut words);
    Ok(Transcript { language, words })
}

/// Garante ordem crescente e sem sobreposição.
fn fix_timings(words: &mut [Word]) {
    for i in 1..words.len() {
        if words[i].start < words[i - 1].start {
            words[i].start = words[i - 1].start;
        }
        if words[i - 1].end > words[i].start {
            words[i - 1].end = words[i].start.max(words[i - 1].start + 0.02);
        }
        if words[i].end < words[i].start {
            words[i].end = words[i].start + 0.05;
        }
    }
}

const CHUNK_SECS: f64 = 1200.0;

async fn transcribe_api(
    cfg: &Config,
    src: &Path,
    work: &Path,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32, &str),
) -> Result<Transcript> {
    if cfg.stt_api_base.trim().is_empty() {
        bail!("Configure o endereço da API de transcrição.");
    }
    progress(0.0, "Extraindo áudio");
    let chunks_dir = work.join("stt_chunks");
    let _ = tokio::fs::remove_dir_all(&chunks_dir).await;
    tokio::fs::create_dir_all(&chunks_dir).await?;
    let mut c = crate::proc::command(paths::ffmpeg());
    c.args(["-hide_banner", "-y", "-i"]).arg(src);
    c.args([
        "-vn",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "48k",
        "-f",
        "segment",
    ]);
    c.args([
        "-segment_time",
        &CHUNK_SECS.to_string(),
        "-reset_timestamps",
        "1",
    ]);
    c.arg(chunks_dir.join("chunk_%03d.mp3"));
    proc::run(c, cancel, |_| {})
        .await
        .context("falha ao extrair o áudio")?;

    let mut files: Vec<_> = std::fs::read_dir(&chunks_dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    let client = crate::models::http_client();
    let url = format!(
        "{}/audio/transcriptions",
        cfg.stt_api_base.trim_end_matches('/')
    );
    let mut all = Transcript::default();
    let n = files.len().max(1);
    for (i, f) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            bail!("Cancelado");
        }
        progress(
            i as f32 / n as f32,
            &format!("Transcrevendo via API ({}/{})", i + 1, n),
        );
        let offset = i as f64 * CHUNK_SECS;
        let bytes = tokio::fs::read(f).await?;
        let mut last_err = None;
        let mut result = None;
        for attempt in 0..3 {
            let part = reqwest::multipart::Part::bytes(bytes.clone())
                .file_name("audio.mp3")
                .mime_str("audio/mpeg")?;
            let mut form = reqwest::multipart::Form::new()
                .part("file", part)
                .text("model", cfg.stt_api_model.clone())
                .text("response_format", "verbose_json")
                .text("timestamp_granularities[]", "word")
                .text("timestamp_granularities[]", "segment");
            if cfg.language != "auto" && !cfg.language.is_empty() {
                form = form.text("language", cfg.language.clone());
            }
            let mut req = client
                .post(&url)
                .multipart(form)
                .timeout(std::time::Duration::from_secs(900));
            if !cfg.stt_api_key.is_empty() {
                req = req.bearer_auth(&cfg.stt_api_key);
            }
            match req.send().await {
                Ok(r) if r.status().is_success() => {
                    result = Some(r.json::<serde_json::Value>().await?);
                    break;
                }
                Ok(r) => {
                    let st = r.status();
                    let body = r.text().await.unwrap_or_default();
                    last_err = Some(anyhow::anyhow!(
                        "API de transcrição respondeu {st}: {}",
                        body.chars().take(600).collect::<String>()
                    ));
                    if st.as_u16() != 429 && !st.is_server_error() {
                        break;
                    }
                }
                Err(e) => last_err = Some(e.into()),
            }
            tokio::time::sleep(std::time::Duration::from_secs(5 * (attempt + 1))).await;
        }
        let v = match result {
            Some(v) => v,
            None => {
                return Err(
                    last_err.unwrap_or_else(|| anyhow::anyhow!("falha na API de transcrição"))
                );
            }
        };
        if all.language.is_empty() {
            all.language = v["language"].as_str().unwrap_or("").to_string();
        }
        let words = v["words"].as_array().cloned().unwrap_or_default();
        if !words.is_empty() {
            for w in words {
                let text = w["word"].as_str().or(w["text"].as_str()).unwrap_or("");
                if let Some(t) = clean_word(text) {
                    let s = w["start"].as_f64().unwrap_or(0.0) + offset;
                    let e = w["end"].as_f64().unwrap_or(0.0) + offset;
                    all.words.push(Word {
                        start: s,
                        end: e.max(s + 0.05),
                        text: t,
                    });
                }
            }
            // A API costuma devolver palavras sem pontuação; recupera a pontuação dos segmentos.
            restore_punctuation(
                &mut all.words,
                v["segments"]
                    .as_array()
                    .map(|a| a.as_slice())
                    .unwrap_or(&[]),
                offset,
            );
        } else {
            // Sem palavras: distribui o tempo do segmento entre as palavras.
            for s in v["segments"].as_array().cloned().unwrap_or_default() {
                let st = s["start"].as_f64().unwrap_or(0.0) + offset;
                let en = s["end"].as_f64().unwrap_or(0.0) + offset;
                let toks: Vec<&str> = s["text"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                let total: usize = toks
                    .iter()
                    .map(|t| t.chars().count() + 1)
                    .sum::<usize>()
                    .max(1);
                let mut t = st;
                for tok in toks {
                    let d = (en - st) * (tok.chars().count() + 1) as f64 / total as f64;
                    if let Some(txt) = clean_word(tok) {
                        all.words.push(Word {
                            start: t,
                            end: t + d,
                            text: txt,
                        });
                    }
                    t += d;
                }
            }
        }
    }
    let _ = tokio::fs::remove_dir_all(&chunks_dir).await;
    fix_timings(&mut all.words);
    Ok(all)
}

/// Copia a pontuação final dos segmentos para as palavras correspondentes.
fn restore_punctuation(words: &mut [Word], segments: &[serde_json::Value], offset: f64) {
    for s in segments {
        let text = s["text"].as_str().unwrap_or("").trim();
        let Some(last_char) = text.chars().last() else {
            continue;
        };
        if !matches!(last_char, '.' | '?' | '!' | ',' | '…') {
            continue;
        }
        let end = s["end"].as_f64().unwrap_or(0.0) + offset;
        if let Some(w) = words
            .iter_mut()
            .filter(|w| (w.end - end).abs() < 1.0)
            .min_by(|a, b| (a.end - end).abs().total_cmp(&(b.end - end).abs()))
            && !w.text.ends_with(['.', '?', '!', ',', '…'])
        {
            w.text.push(last_char);
        }
    }
}

//! Escolha dos melhores trechos: IA (API/local) ou heurística sem IA.

use crate::config::Config;
use crate::llm::{Llm, extract_json};
use crate::transcribe::{Segment, Transcript};
use anyhow::{Result, bail};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub score: u32,
    pub reason: String,
}

const SYSTEM: &str = "Você é um editor profissional de vídeos curtos (TikTok, Reels, YouTube Shorts). \
Sua tarefa é encontrar, na transcrição, os trechos com maior potencial de viralizar. \
Um bom clipe: começa com um gancho forte nos primeiros 3 segundos, faz sentido sozinho (sem precisar do resto do vídeo), \
tem uma ideia completa (começo, meio e fim), e traz emoção, humor, polêmica, uma revelação, uma dica valiosa ou uma história marcante. \
Nunca comece um clipe no meio de uma frase. Responda SOMENTE com JSON válido.";

fn user_prompt(cfg: &Config, lines: &str, n: usize) -> String {
    let extra = if cfg.extra_prompt.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\nInstruções extras do usuário: {}\n",
            cfg.extra_prompt.trim()
        )
    };
    format!(
        "Transcrição (cada linha: [início-fim em segundos] texto):\n{lines}\n\n\
Escolha até {n} clipes. Regras:\n\
- Cada clipe deve durar entre {min:.0} e {max:.0} segundos.\n\
- \"start\" deve ser o início de uma linha e \"end\" o fim de uma linha, usando exatamente os números mostrados.\n\
- Os clipes não podem se sobrepor.\n\
- \"title\": título viral em forma de gancho (máx. 55 caracteres), que desperte curiosidade sobre o assunto do clipe, \
no MESMO idioma da transcrição. Ex.: \"Ninguém te conta isso sobre dinheiro\", \"O erro que quase acabou com tudo\". Sem hashtags nem emojis.\n\
- \"score\": nota de 0 a 100 do potencial viral.\n\
- \"reason\": uma frase curta explicando por que o trecho é bom.\n{extra}\n\
Formato da resposta:\n\
{{\"clips\":[{{\"start\":12.3,\"end\":48.9,\"title\":\"...\",\"score\":87,\"reason\":\"...\"}}]}}",
        min = cfg.min_len,
        max = cfg.max_len,
    )
}

fn parse_time(v: &Value) -> Option<f64> {
    if let Some(f) = v.as_f64() {
        return Some(f);
    }
    let s = v.as_str()?.trim();
    if let Ok(f) = s.parse::<f64>() {
        return Some(f);
    }
    // Formatos mm:ss ou hh:mm:ss
    let parts: Vec<f64> = s
        .split(':')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    Some(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p))
}

fn parse_candidates(v: &Value) -> Vec<Candidate> {
    let arr = if let Some(a) = v.as_array() {
        a.clone()
    } else {
        v.get("clips")
            .or_else(|| v.get("clipes"))
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default()
    };
    arr.iter()
        .filter_map(|c| {
            let start = parse_time(c.get("start")?)?;
            let end = parse_time(c.get("end")?)?;
            let score = c
                .get("score")
                .and_then(|s| s.as_f64().or_else(|| s.as_str()?.parse().ok()))
                .unwrap_or(50.0);
            Some(Candidate {
                start,
                end,
                title: c
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(90)
                    .collect(),
                score: score.clamp(0.0, 100.0) as u32,
                reason: c
                    .get("reason")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
            })
        })
        .filter(|c| c.end > c.start)
        .collect()
}

/// Ajusta início/fim para limites de frases e respeita a duração mínima/máxima.
fn snap(c: &mut Candidate, segs: &[Segment], cfg: &Config) {
    if segs.is_empty() {
        return;
    }
    // início: frase cujo início está mais próximo
    let si = segs
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1.start - c.start)
                .abs()
                .total_cmp(&(b.1.start - c.start).abs())
        })
        .map(|x| x.0)
        .unwrap();
    let mut ei = segs
        .iter()
        .enumerate()
        .skip(si)
        .min_by(|a, b| (a.1.end - c.end).abs().total_cmp(&(b.1.end - c.end).abs()))
        .map(|x| x.0)
        .unwrap_or(si);
    while segs[ei].end - segs[si].start < cfg.min_len && ei + 1 < segs.len() {
        ei += 1;
    }
    while segs[ei].end - segs[si].start > cfg.max_len && ei > si {
        ei -= 1;
    }
    c.start = segs[si].start;
    c.end = segs[ei].end;
    // frase única longa demais: corta no máximo permitido
    if c.end - c.start > cfg.max_len {
        c.end = c.start + cfg.max_len;
    }
    // pequena folga para não cortar a última sílaba
    c.start = (c.start - 0.15).max(0.0);
    c.end += 0.35;
}

fn overlap(a: &Candidate, b: &Candidate) -> f64 {
    let inter = (a.end.min(b.end) - a.start.max(b.start)).max(0.0);
    inter / (a.end - a.start).min(b.end - b.start).max(0.01)
}

fn pick(mut cands: Vec<Candidate>, max: usize) -> Vec<Candidate> {
    cands.sort_by_key(|c| std::cmp::Reverse(c.score));
    let mut out: Vec<Candidate> = Vec::new();
    for c in cands {
        if out.len() >= max {
            break;
        }
        if out.iter().all(|o| overlap(o, &c) < 0.3) {
            out.push(c);
        }
    }
    out
}

fn fallback_title(t: &Transcript, c: &Candidate) -> String {
    let text = t.text_between(c.start, c.end);
    let words: Vec<&str> = text.split_whitespace().take(8).collect();
    let mut s = words.join(" ");
    s = s.trim_end_matches([',', '.', ';', ':']).to_string();
    if text.split_whitespace().count() > 8 {
        s.push('…');
    }
    s
}

pub async fn find(
    cfg: &Config,
    t: &Transcript,
    duration: f64,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32, &str),
) -> Result<Vec<Candidate>> {
    let segs = t.segments();
    if segs.is_empty() {
        bail!("Nenhuma fala foi encontrada no vídeo.");
    }
    let max = cfg.max_clips.max(1) as usize;
    let llm = match cfg.llm_provider.as_str() {
        "none" | "" => None,
        _ => {
            progress(
                0.0,
                if cfg.llm_provider == "local" {
                    "Carregando modelo de IA local"
                } else {
                    "Conectando à IA"
                },
            );
            Llm::from_config(cfg, cancel).await?
        }
    };
    let mut cands = match llm {
        Some(llm) => {
            // Divide a transcrição em blocos que cabem no contexto do modelo.
            let chunk = if llm.is_local {
                (cfg.llm_ctx as f64 * 0.075).clamp(240.0, 1500.0)
            } else {
                1500.0
            };
            let step = (chunk - cfg.max_len).max(chunk * 0.5);
            let mut windows = Vec::new();
            let mut w0 = 0.0;
            loop {
                windows.push((w0, w0 + chunk));
                if w0 + chunk >= duration {
                    break;
                }
                w0 += step;
            }
            let per =
                ((max as f64 * chunk / duration.max(1.0)).ceil() as usize + 1).clamp(2, max.max(2));
            let mut all = Vec::new();
            let mut errors = Vec::new();
            for (i, (a, b)) in windows.iter().enumerate() {
                if cancel.is_cancelled() {
                    bail!("Cancelado");
                }
                progress(
                    i as f32 / windows.len() as f32,
                    &format!("Analisando com IA ({}/{})", i + 1, windows.len()),
                );
                let lines: Vec<String> = segs
                    .iter()
                    .filter(|s| s.start >= *a && s.start < *b)
                    .map(|s| format!("[{:.1}-{:.1}] {}", s.start, s.end, s.text))
                    .collect();
                if lines.len() < 2 {
                    continue;
                }
                let prompt = user_prompt(cfg, &lines.join("\n"), per);
                match llm.chat(SYSTEM, &prompt, true).await {
                    Ok(resp) => match extract_json(&resp) {
                        Some(v) => all.extend(parse_candidates(&v)),
                        None => errors.push("resposta da IA sem JSON válido".to_string()),
                    },
                    Err(e) => {
                        if windows.len() == 1 || errors.len() >= 2 {
                            return Err(e);
                        }
                        errors.push(e.to_string());
                    }
                }
            }
            if all.is_empty() {
                eprintln!("IA não retornou clipes ({errors:?}); usando heurística");
                heuristic(cfg, &segs, max)
            } else {
                all
            }
        }
        None => heuristic(cfg, &segs, max),
    };
    for c in cands.iter_mut() {
        c.start = c.start.clamp(0.0, duration);
        c.end = c.end.clamp(0.0, duration);
        snap(c, &segs, cfg);
        c.end = c.end.min(duration);
        if c.title.is_empty() {
            c.title = fallback_title(t, c);
        }
    }
    cands.retain(|c| c.end - c.start >= 3.0);
    progress(1.0, "Trechos escolhidos");
    Ok(pick(cands, max))
}

/// Sem IA: pontua janelas por densidade de fala, perguntas/exclamações e pausas naturais.
fn heuristic(cfg: &Config, segs: &[Segment], max: usize) -> Vec<Candidate> {
    let target = ((cfg.min_len + cfg.max_len) / 2.0).max(cfg.min_len);
    let hooks = [
        "segredo",
        "verdade",
        "ninguém",
        "nunca",
        "sempre",
        "incrível",
        "imagina",
        "olha",
        "cuidado",
        "dica",
        "erro",
        "secret",
        "never",
        "always",
        "truth",
        "nobody",
        "crazy",
        "imagine",
        "mistake",
        "why",
        "how",
        "porque",
        "como",
    ];
    let mut out = Vec::new();
    for i in 0..segs.len() {
        let mut j = i;
        while j + 1 < segs.len() && segs[j].end - segs[i].start < target {
            j += 1;
        }
        let dur = segs[j].end - segs[i].start;
        if dur < cfg.min_len * 0.8 || dur > cfg.max_len * 1.2 {
            continue;
        }
        let text: String = segs[i..=j]
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let lower = text.to_lowercase();
        let words = text.split_whitespace().count() as f64;
        let wps = words / dur.max(1.0);
        let mut score = (wps / 3.0 * 45.0).min(55.0);
        score += (text.matches('?').count() as f64 * 6.0).min(18.0);
        score += (text.matches('!').count() as f64 * 4.0).min(12.0);
        let first = segs[i].text.to_lowercase();
        score += hooks.iter().filter(|h| first.contains(*h)).count() as f64 * 6.0;
        score += (hooks.iter().filter(|h| lower.contains(*h)).count() as f64 * 2.0).min(10.0);
        out.push(Candidate {
            start: segs[i].start,
            end: segs[j].end,
            title: String::new(),
            score: score.clamp(1.0, 99.0) as u32,
            reason: "Escolhido automaticamente (sem IA) pela densidade e ritmo da fala.".into(),
        });
    }
    pick(out, max * 3)
}

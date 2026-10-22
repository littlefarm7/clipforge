//! Escolha dos melhores trechos: IA (API/local) ou heurística sem IA.

use crate::config::Config;
use crate::llm::{ContextOverflow, Llm, extract_json};
use crate::transcribe::{Segment, Transcript};
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::VecDeque;
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

fn user_prompt(cfg: &Config, lines: &str, n: usize, exclude: &[(f64, f64)]) -> String {
    let extra = if cfg.extra_prompt.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\nInstruções extras do usuário: {}\n",
            cfg.extra_prompt.trim()
        )
    };
    let taken = if exclude.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = exclude
            .iter()
            .map(|(a, b)| format!("[{}-{}]", a.floor() as u64, b.ceil() as u64))
            .collect();
        format!(
            "- Estes trechos JÁ foram escolhidos; não os repita nem se sobreponha a eles: {}\n",
            list.join(", ")
        )
    };
    format!(
        "Transcrição (cada linha: [início-fim em segundos] texto):\n{lines}\n\n\
Escolha EXATAMENTE {n} clipes diferentes desta transcrição. Mesmo que nem todos sejam excelentes, \
devolva {n} clipes, ordenando pela qualidade com a nota (\"score\"). Regras:\n\
- Cada clipe deve durar entre {min:.0} e {max:.0} segundos.\n\
- \"start\" deve ser o início de uma linha e \"end\" o fim de uma linha, usando exatamente os números mostrados.\n\
- Os clipes não podem se sobrepor.\n{taken}\
- \"title\": título viral em forma de gancho (máx. 55 caracteres), que desperte curiosidade sobre o assunto do clipe, \
no MESMO idioma da transcrição. Ex.: \"Ninguém te conta isso sobre dinheiro\", \"O erro que quase acabou com tudo\". Sem hashtags nem emojis.\n\
- \"score\": nota de 0 a 100 do potencial viral.\n\
- \"reason\": uma frase curta explicando por que o trecho é bom.\n{extra}\n\
Responda só com o JSON, neste formato:\n\
{{\"clips\":[{{\"start\":12,\"end\":49,\"title\":\"...\",\"score\":87,\"reason\":\"...\"}}]}}",
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

/// Linha da transcrição enviada à IA. Segundos inteiros economizam tokens;
/// os limites são reajustados às frases depois (função `snap`).
fn line(s: &Segment) -> String {
    format!(
        "[{}-{}] {}",
        s.start.floor() as u64,
        s.end.ceil() as u64,
        s.text
    )
}

/// Estimativa de tokens a partir de quantos caracteres cabem em um token.
fn est_tokens(s: &str, chars_per_token: f64) -> usize {
    (s.chars().count() as f64 / chars_per_token).ceil() as usize + 1
}

/// Divide as frases em blocos [início, fim) que cabem em `budget` tokens.
/// Blocos vizinhos se sobrepõem por `overlap` segundos para não perder trechos na fronteira.
fn chunks(
    segs: &[Segment],
    budget: usize,
    chars_per_token: f64,
    overlap: f64,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut a = 0;
    while a < segs.len() {
        let mut b = a;
        let mut used = 0;
        while b < segs.len() {
            let t = est_tokens(&line(&segs[b]), chars_per_token);
            if used + t > budget && b > a {
                break;
            }
            used += t;
            b += 1;
        }
        out.push((a, b));
        if b >= segs.len() {
            break;
        }
        let mut next = b;
        while next > a + 1 && segs[b - 1].end - segs[next - 1].start < overlap {
            next -= 1;
        }
        a = next;
    }
    out
}

/// Ajusta os candidatos às frases e à duração do vídeo.
fn finish(
    mut cands: Vec<Candidate>,
    segs: &[Segment],
    cfg: &Config,
    duration: f64,
) -> Vec<Candidate> {
    for c in cands.iter_mut() {
        c.start = c.start.clamp(0.0, duration);
        c.end = c.end.clamp(0.0, duration);
        snap(c, segs, cfg);
        c.end = c.end.min(duration);
    }
    cands.retain(|c| c.end - c.start >= 3.0);
    cands
}

/// Acrescenta a `chosen` os melhores candidatos de `extra` que não se sobrepõem, até `target`.
fn fill(chosen: &mut Vec<Candidate>, mut extra: Vec<Candidate>, target: usize) {
    extra.sort_by_key(|c| std::cmp::Reverse(c.score));
    for c in extra {
        if chosen.len() >= target {
            break;
        }
        if chosen.iter().all(|o| overlap(o, &c) < 0.15) {
            chosen.push(c);
        }
    }
}

/// Pede clipes à IA bloco a bloco. Blocos que não cabem no contexto são divididos ao meio.
/// `per_block` diz quantos clipes pedir em cada bloco; `exclude` lista trechos já escolhidos.
#[allow(clippy::too_many_arguments)]
async fn ask_blocks(
    llm: &Llm,
    cfg: &Config,
    segs: &[Segment],
    blocks: Vec<(usize, usize)>,
    per_block: &(dyn Fn(f64) -> usize + Sync),
    exclude: &[(f64, f64)],
    cancel: &CancellationToken,
    progress: &mut (dyn FnMut(f32, &str) + Send),
    label: &str,
) -> Result<(Vec<Candidate>, Vec<String>)> {
    let mut queue: VecDeque<(usize, usize)> = blocks.into();
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let mut done = 0usize;
    while let Some((a, b)) = queue.pop_front() {
        if cancel.is_cancelled() {
            bail!("Cancelado");
        }
        let total = done + queue.len() + 1;
        progress(
            done as f32 / total as f32,
            &format!("{label} ({}/{})", done + 1, total),
        );
        let part = &segs[a..b];
        let span = part[part.len() - 1].end - part[0].start;
        if part.len() < 2 || span < cfg.min_len * 0.8 {
            done += 1;
            continue;
        }
        let ex: Vec<(f64, f64)> = exclude
            .iter()
            .copied()
            .filter(|(s, e)| *e > part[0].start && *s < part[part.len() - 1].end)
            .collect();
        let lines: Vec<String> = part.iter().map(line).collect();
        let prompt = user_prompt(cfg, &lines.join("\n"), per_block(span), &ex);
        // No modelo local, confere o tamanho exato antes de enviar
        let too_big = if llm.is_local {
            match llm.count_tokens(&format!("{SYSTEM}\n{prompt}")).await {
                Some(n) => n > llm.prompt_limit(),
                None => false,
            }
        } else {
            false
        };
        let res = if too_big {
            Err(ContextOverflow.into())
        } else {
            llm.chat(SYSTEM, &prompt, true).await
        };
        match res {
            Ok(resp) => match extract_json(&resp) {
                Some(v) => found.extend(parse_candidates(&v)),
                None => errors.push("a IA respondeu sem a lista de clipes em JSON".to_string()),
            },
            // Não coube no contexto: divide o bloco ao meio e tenta de novo
            Err(e) if e.is::<ContextOverflow>() => {
                if b - a >= 2 {
                    let mid = a + (b - a) / 2;
                    queue.push_front((mid, b));
                    queue.push_front((a, mid));
                    continue;
                }
                errors.push(e.to_string());
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
        done += 1;
    }
    Ok((found, errors))
}

pub async fn find(
    cfg: &Config,
    t: &Transcript,
    duration: f64,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32, &str) + Send,
) -> Result<(Vec<Candidate>, Option<String>)> {
    let segs = t.segments();
    if segs.is_empty() {
        bail!("Nenhuma fala foi encontrada no vídeo.");
    }
    // Quantos clipes sem sobreposição cabem no vídeo
    let feasible = ((duration / cfg.min_len.max(5.0)).floor() as usize).max(1);
    let target = (cfg.max_clips.max(1) as usize).min(feasible);
    let mut notice = None;
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
            match Llm::from_config(cfg, cancel).await {
                Ok(l) => l,
                Err(e) => {
                    if cancel.is_cancelled() {
                        return Err(e);
                    }
                    // A IA não pôde ser usada: segue com a escolha automática
                    eprintln!("IA indisponível: {e:#}");
                    notice = Some(crate::friendly::ai_notice(&format!("{e:#}")));
                    None
                }
            }
        }
    };
    let mut chosen: Vec<Candidate> = Vec::new();
    if let Some(llm) = llm {
        // Mede o tamanho dos tokens deste modelo com um pedaço da própria transcrição
        let sample: String = segs
            .iter()
            .take(120)
            .map(line)
            .collect::<Vec<_>>()
            .join("\n");
        let cpt = llm.chars_per_token(&sample).await;
        let blocks = chunks(&segs, llm.input_budget(), cpt, cfg.max_len * 0.5);
        // Cada bloco recebe uma parte proporcional dos clipes, com folga para a IA escolher
        let per_block = |span: f64| {
            ((target as f64 * span / duration.max(1.0)).ceil() as usize + 2).clamp(2, target + 2)
        };
        let (found, mut errors) = ask_blocks(
            &llm,
            cfg,
            &segs,
            blocks.clone(),
            &per_block,
            &[],
            cancel,
            &mut progress,
            "Analisando com IA",
        )
        .await?;
        if found.is_empty() && !errors.is_empty() {
            eprintln!("a IA não conseguiu analisar o vídeo: {}", errors[0]);
            notice = Some(crate::friendly::ai_notice(&errors[0]));
        }
        fill(&mut chosen, finish(found, &segs, cfg, duration), target);

        // Rodadas extras: se faltaram clipes, pede trechos diferentes dos já escolhidos
        for _ in 0..2 {
            if chosen.len() >= target {
                break;
            }
            let before = chosen.len();
            let need = target - chosen.len();
            let taken: Vec<(f64, f64)> = chosen.iter().map(|c| (c.start, c.end)).collect();
            let per_more = |span: f64| {
                ((need as f64 * span / duration.max(1.0)).ceil() as usize + 1).clamp(1, need + 1)
            };
            let (more, errs) = ask_blocks(
                &llm,
                cfg,
                &segs,
                blocks.clone(),
                &per_more,
                &taken,
                cancel,
                &mut progress,
                "Buscando mais trechos",
            )
            .await?;
            errors.extend(errs);
            fill(&mut chosen, finish(more, &segs, cfg, duration), target);
            if chosen.len() == before {
                break;
            }
        }
        if !errors.is_empty() {
            eprintln!("avisos da IA: {errors:?}");
        }
    }
    // Completa com a heurística quando a IA não trouxe trechos suficientes (ou está desligada)
    if chosen.len() < target {
        let extra = finish(heuristic(cfg, &segs, target), &segs, cfg, duration);
        fill(&mut chosen, extra, target);
    }
    for c in chosen.iter_mut() {
        if c.title.is_empty() {
            c.title = fallback_title(t, c);
        }
    }
    chosen.sort_by_key(|c| std::cmp::Reverse(c.score));
    progress(1.0, "Trechos escolhidos");
    Ok((chosen, notice))
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
    pick(out, max * 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(i: usize) -> Segment {
        let start = i as f64 * 5.0;
        Segment {
            start,
            end: start + 4.5,
            text: "uma frase de exemplo com várias palavras para testar o tamanho do bloco".into(),
            first: i,
            last: i,
        }
    }

    #[test]
    fn blocos_respeitam_o_limite_e_cobrem_tudo() {
        let segs: Vec<Segment> = (0..2000).map(seg).collect();
        let budget = 5400; // contexto 8192 menos a reserva
        let cs = chunks(&segs, budget, 1.3, 30.0);
        assert!(cs.len() > 1);
        assert_eq!(cs[0].0, 0);
        assert_eq!(cs.last().unwrap().1, segs.len());
        for w in cs.windows(2) {
            // sem buracos entre blocos (há sobreposição)
            assert!(w[1].0 < w[0].1);
            assert!(w[1].0 > w[0].0);
        }
        for &(a, b) in &cs {
            let tokens: usize = segs[a..b].iter().map(|s| est_tokens(&line(s), 1.3)).sum();
            assert!(tokens <= budget, "bloco com {tokens} tokens");
        }
    }

    #[test]
    fn transcricao_curta_vira_um_bloco() {
        let segs: Vec<Segment> = (0..10).map(seg).collect();
        assert_eq!(chunks(&segs, 6000, 2.5, 30.0), vec![(0, 10)]);
    }
}

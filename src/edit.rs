//! Motor de edição das estruturas (Podcast Turbo, Conversa, React, Cinemático, Meme,
//! Top Momentos).
//!
//! Etapas:
//! 1. Trechos: o que entra no clipe (sem as pausas, quando pedido; vários momentos no Top).
//! 2. Rostos: quem fala e onde está cada pessoa (`face::analyze`).
//! 3. Planos: cada trecho vira um ou mais planos (quem fala, reação, zoom de ênfase).
//! 4. Montagem: um único ffmpeg corta, enquadra, junta, monta o layout da estrutura,
//!    desenha as legendas, mistura a música e ajusta o volume.

use crate::captions::{self, OWord, Scene};
use crate::config::Config;
use crate::face::{self, ANALYSIS_FPS, Analysis};
use crate::render::{self, RenderJob};
use crate::structures::{self, StyleOpts};
use crate::{paths, proc};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use tokio_util::sync::CancellationToken;

/// Tela preta entre os momentos do Top Momentos (segundos).
const SEP: f64 = 0.3;

/// Trecho do vídeo original (segundos absolutos) que entra no clipe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Part {
    /// Momento (entrada do ffmpeg) de onde o trecho vem
    pub moment: usize,
    pub a: f64,
    pub b: f64,
}

/// Como um plano é enquadrado.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum View {
    /// Imagem inteira (as estruturas de React montam o layout depois)
    Raw,
    /// Recorte vertical: rosto em (`cx`, `cy`) do vídeo, com `zoom`
    Crop { cx: f64, cy: f64, zoom: f64 },
}

/// Um plano: pedaço de um trecho com o seu enquadramento.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shot {
    pub part: usize,
    pub a: f64,
    pub b: f64,
    pub view: View,
    /// Plano de reação (câmera em quem escuta)
    pub reaction: bool,
}

// ---------------------------------------------------------------------------
// 1. Trechos
// ---------------------------------------------------------------------------

/// Arredonda para um quadro exato (os cortes de áudio e vídeo ficam iguais).
fn align(t: f64, fps: f64) -> f64 {
    (t * fps).round() / fps
}

/// Trechos de fala de um momento. Com `cut_pauses`, pausas maiores que `max_gap` saem.
pub fn speech_parts(
    words: &[(f64, f64)],
    moment: usize,
    start: f64,
    end: f64,
    cut_pauses: bool,
    max_gap: f64,
    pad: f64,
) -> Vec<Part> {
    let inside: Vec<(f64, f64)> = words
        .iter()
        .copied()
        .filter(|(a, b)| *b > start && *a < end)
        .collect();
    if !cut_pauses || inside.is_empty() {
        return vec![Part {
            moment,
            a: start,
            b: end,
        }];
    }
    let mut runs: Vec<(f64, f64)> = Vec::new();
    for (a, b) in inside {
        match runs.last_mut() {
            Some(r) if a - r.1 <= max_gap => r.1 = r.1.max(b),
            _ => runs.push((a, b)),
        }
    }
    let mut parts: Vec<Part> = Vec::new();
    for (a, b) in runs {
        let (a, b) = ((a - pad).max(start), (b + pad).min(end));
        match parts.last_mut() {
            Some(p) if a <= p.b + 0.02 => p.b = p.b.max(b),
            _ => parts.push(Part { moment, a, b }),
        }
    }
    parts.retain(|p| p.b - p.a >= 0.25);
    if parts.is_empty() {
        parts.push(Part {
            moment,
            a: start,
            b: end,
        });
    }
    parts
}

// ---------------------------------------------------------------------------
// 2. Rostos
// ---------------------------------------------------------------------------

/// Rostos de um momento: análise a 10 quadros/s a partir de `t0`.
pub struct Faces {
    pub t0: f64,
    pub a: Analysis,
    pub speakers: Vec<Option<usize>>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// Junta as "trilhas" que são a mesma pessoa. O rastreador cria uma trilha nova cada vez
/// que perde um rosto por um instante; aqui elas viram uma pessoa só quando ficam no mesmo
/// lugar, com o mesmo tamanho, e nunca aparecem ao mesmo tempo.
pub fn merge_tracks(a: &mut Analysis) {
    let n = a.tracks;
    if n == 0 {
        return;
    }
    let mut stats: Vec<(Vec<f64>, Vec<f64>, Vec<usize>)> = vec![(vec![], vec![], vec![]); n];
    for (fi, f) in a.frames.iter().enumerate() {
        for s in &f.faces {
            stats[s.track].0.push(s.cx);
            stats[s.track].1.push(s.w);
            stats[s.track].2.push(fi);
        }
    }
    let mut order: Vec<usize> = (0..n).filter(|t| !stats[*t].2.is_empty()).collect();
    order.sort_by_key(|t| std::cmp::Reverse(stats[*t].2.len()));
    // Pessoas: (posição, largura, quadros em que aparece)
    let mut people: Vec<(f64, f64, Vec<bool>)> = Vec::new();
    let mut map = vec![usize::MAX; n];
    for t in order {
        let cx = median(stats[t].0.clone()).unwrap_or(0.5);
        let w = median(stats[t].1.clone()).unwrap_or(0.1);
        let found = people.iter().position(|(pcx, pw, frames)| {
            (pcx - cx).abs() < (0.6 * pw.max(w)).max(0.05)
                && (w / pw) > 0.6
                && (w / pw) < 1.65
                && stats[t].2.iter().all(|fi| !frames[*fi])
        });
        let id = match found {
            Some(i) => i,
            None => {
                people.push((cx, w, vec![false; a.frames.len()]));
                people.len() - 1
            }
        };
        for fi in &stats[t].2 {
            people[id].2[*fi] = true;
        }
        map[t] = id;
    }
    for f in a.frames.iter_mut() {
        for s in f.faces.iter_mut() {
            s.track = map[s.track];
        }
    }
    a.tracks = people.len();
}

impl Faces {
    fn idx(&self, t: f64) -> usize {
        (((t - self.t0) * ANALYSIS_FPS).round().max(0.0) as usize)
            .min(self.a.frames.len().saturating_sub(1))
    }

    fn range(&self, a: f64, b: f64) -> std::ops::Range<usize> {
        if self.a.frames.is_empty() {
            return 0..0;
        }
        let (i, j) = (self.idx(a), self.idx(b));
        i..(j.max(i + 1)).min(self.a.frames.len())
    }

    pub fn speaker_at(&self, t: f64) -> Option<usize> {
        self.speakers.get(self.idx(t)).copied().flatten()
    }

    /// Posição mediana (cx, cy, largura) de uma pessoa num intervalo.
    pub fn pos(&self, track: usize, a: f64, b: f64) -> Option<(f64, f64, f64)> {
        let seen: Vec<_> = self.a.frames[self.range(a, b)]
            .iter()
            .flat_map(|f| f.faces.iter())
            .filter(|s| s.track == track)
            .copied()
            .collect();
        Some((
            median(seen.iter().map(|s| s.cx).collect())?,
            median(seen.iter().map(|s| s.cy).collect())?,
            median(seen.iter().map(|s| s.w).collect())?,
        ))
    }

    /// Pessoa que mais aparece num intervalo (opcionalmente, diferente de `not`).
    pub fn most_seen(&self, a: f64, b: f64, not: Option<usize>) -> Option<usize> {
        let mut count: Vec<(usize, usize)> = Vec::new();
        for f in &self.a.frames[self.range(a, b)] {
            for s in &f.faces {
                if Some(s.track) == not {
                    continue;
                }
                match count.iter_mut().find(|c| c.0 == s.track) {
                    Some(c) => c.1 += 1,
                    None => count.push((s.track, 1)),
                }
            }
        }
        let frames = self.range(a, b).len().max(1);
        count
            .into_iter()
            .filter(|c| c.1 * 3 >= frames) // visível em pelo menos 1/3 do intervalo
            .max_by_key(|c| c.1)
            .map(|c| c.0)
    }

    /// Cortes de câmera do vídeo original dentro de (a, b), em segundos.
    fn cuts(&self, a: f64, b: f64) -> Vec<f64> {
        self.range(a, b)
            .filter(|&i| self.a.frames[i].cut)
            .map(|i| self.t0 + i as f64 / ANALYSIS_FPS)
            .filter(|t| *t > a + 0.05 && *t < b - 0.05)
            .collect()
    }

    /// A pessoa principal (quem mais aparece no momento todo).
    pub fn main_face(&self) -> Option<(f64, f64, f64)> {
        let t1 = self.t0 + self.a.frames.len() as f64 / ANALYSIS_FPS;
        let t = self.most_seen(self.t0, t1, None).or_else(|| {
            // Mesmo que apareça pouco, usa o rosto mais frequente
            let mut c: Vec<(usize, usize)> = Vec::new();
            for s in self.a.frames.iter().flat_map(|f| f.faces.iter()) {
                match c.iter_mut().find(|x| x.0 == s.track) {
                    Some(x) => x.1 += 1,
                    None => c.push((s.track, 1)),
                }
            }
            c.into_iter().max_by_key(|x| x.1).map(|x| x.0)
        })?;
        self.pos(t, self.t0, t1)
    }
}

// ---------------------------------------------------------------------------
// 3. Planos
// ---------------------------------------------------------------------------

/// Parâmetros dos planos de cada estrutura.
struct ShotRules {
    min_shot: f64,
    base_zoom: f64,
    react_min: f64,
    react_len: f64,
}

fn shot_rules(structure: &str) -> ShotRules {
    match structure {
        "conversa" => ShotRules {
            min_shot: 1.4,
            base_zoom: 1.0,
            react_min: 6.0,
            react_len: 1.6,
        },
        "cinema" => ShotRules {
            min_shot: 0.9,
            base_zoom: 1.35,
            react_min: 99.0,
            react_len: 0.0,
        },
        "top" => ShotRules {
            min_shot: 1.0,
            base_zoom: 1.06,
            react_min: 99.0,
            react_len: 0.0,
        },
        _ => ShotRules {
            min_shot: 0.7,
            base_zoom: 1.0,
            react_min: 4.0,
            react_len: 0.9,
        },
    }
}

/// Planos de um trecho: segue quem fala, corta nos cortes do vídeo original e, em falas
/// longas, mostra a reação de quem escuta.
fn part_shots(
    pi: usize,
    p: &Part,
    faces: Option<&Faces>,
    rules: &ShotRules,
    reactions: bool,
) -> Vec<Shot> {
    let center = View::Crop {
        cx: 0.5,
        cy: 0.45,
        zoom: rules.base_zoom,
    };
    let Some(f) = faces.filter(|f| !f.a.frames.is_empty()) else {
        return vec![Shot {
            part: pi,
            a: p.a,
            b: p.b,
            view: center,
            reaction: false,
        }];
    };
    // Quem fala a cada 0,1 s, com cortes duros nos cortes do vídeo original
    let cuts = f.cuts(p.a, p.b);
    let mut runs: Vec<(f64, f64, Option<usize>, bool)> = Vec::new(); // (a, b, pessoa, começa num corte)
    let step = 1.0 / ANALYSIS_FPS;
    let mut t = p.a;
    while t < p.b - 1e-6 {
        let t1 = (t + step).min(p.b);
        let who = f.speaker_at(t + step / 2.0);
        let hard = cuts.iter().any(|c| *c >= t - 1e-6 && *c < t1 - 1e-6);
        match runs.last_mut() {
            Some(r) if r.2 == who && !hard => r.1 = t1,
            _ => runs.push((t, t1, who, hard)),
        }
        t = t1;
    }
    // Sem informação de quem fala: herda do vizinho (ou quem mais aparece)
    for i in 0..runs.len() {
        if runs[i].2.is_none() {
            runs[i].2 = (if i > 0 { runs[i - 1].2 } else { None })
                .or_else(|| f.most_seen(runs[i].0, runs[i].1, None));
        }
    }
    // Planos curtos demais se juntam ao anterior (a não ser que comecem num corte)
    let mut merged: Vec<(f64, f64, Option<usize>, bool)> = Vec::new();
    for r in runs {
        match merged.last_mut() {
            Some(m) if (r.2 == m.2 || r.1 - r.0 < rules.min_shot) && !r.3 => m.1 = r.1,
            Some(m) if m.1 - m.0 < rules.min_shot && !r.3 => {
                // O anterior era curto: o próximo o absorve
                m.1 = r.1;
                m.2 = r.2;
            }
            _ => merged.push(r),
        }
    }
    // Sem rosto num plano: mantém o enquadramento anterior (ou o rosto principal)
    let main = f.main_face().map(|(cx, cy, _)| View::Crop {
        cx,
        cy,
        zoom: rules.base_zoom,
    });
    let mut last_view = main.unwrap_or(center);
    let mut shots: Vec<Shot> = Vec::new();
    for (a, b, who, _) in merged {
        let fallback = last_view;
        let view_of = |track: Option<usize>, a: f64, b: f64| match track
            .or_else(|| f.most_seen(a, b, None))
            .and_then(|t| f.pos(t, a, b))
        {
            Some((cx, cy, _)) => View::Crop {
                cx,
                cy,
                zoom: rules.base_zoom,
            },
            None => fallback,
        };
        let listener = who.and_then(|w| f.most_seen(a, b, Some(w)));
        if reactions && b - a >= rules.react_min && listener.is_some() {
            // Reação no meio da fala longa
            let r0 = a + (b - a) * 0.55;
            let r1 = (r0 + rules.react_len).min(b - 0.5);
            shots.push(Shot {
                part: pi,
                a,
                b: r0,
                view: view_of(who, a, b),
                reaction: false,
            });
            shots.push(Shot {
                part: pi,
                a: r0,
                b: r1,
                view: view_of(listener, r0, r1),
                reaction: true,
            });
            shots.push(Shot {
                part: pi,
                a: r1,
                b,
                view: view_of(who, a, b),
                reaction: false,
            });
        } else {
            shots.push(Shot {
                part: pi,
                a,
                b,
                view: view_of(who, a, b),
                reaction: false,
            });
        }
        if let Some(last) = shots.iter().rev().find(|s| !s.reaction) {
            last_view = last.view;
        }
    }
    shots
}

/// Zoom de ênfase: alterna o zoom quando o mesmo rosto continua depois de um corte
/// (esconde o pulo) e aproxima nas frases curtas e fortes (com ? ou !).
fn emphasis(shots: &mut [Shot], words: &[(f64, f64, String)], base: f64) {
    let mut prev: Option<(f64, f64, f64)> = None;
    for s in shots.iter_mut() {
        let View::Crop { cx, cy, zoom } = s.view else {
            prev = None;
            continue;
        };
        let mut z = zoom;
        if let Some((px, py, pz)) = prev
            && (px - cx).abs() < 0.08
            && (py - cy).abs() < 0.08
        {
            z = if (pz - base).abs() < 0.01 {
                base * 1.15
            } else {
                base
            };
        }
        let strong = words
            .iter()
            .any(|(a, b, t)| *a >= s.a && *b <= s.b + 0.05 && t.ends_with(['?', '!']));
        if strong && s.b - s.a <= 2.4 && !s.reaction {
            z = base * 1.3;
        }
        s.view = View::Crop { cx, cy, zoom: z };
        prev = Some((cx, cy, z));
    }
}

/// Retângulo do recorte (x, y, largura, altura) em pixels da imagem `sw`×`sh`, com a
/// proporção da saída. O rosto fica na altura `face_y` e na posição horizontal `face_x`.
#[allow(clippy::too_many_arguments)]
pub fn crop_rect(
    cx: f64,
    cy: f64,
    zoom: f64,
    sw: f64,
    sh: f64,
    out_w: f64,
    out_h: f64,
    face_x: f64,
    face_y: f64,
) -> (u32, u32, u32, u32) {
    let aspect = out_w / out_h;
    let (bw, bh) = if sw / sh > aspect {
        (sh * aspect, sh)
    } else {
        (sw, sw / aspect)
    };
    let even = |v: f64| ((v / 2.0).floor() as u32).max(1) * 2;
    let (w, h) = (even(bw / zoom.max(1.0)), even(bh / zoom.max(1.0)));
    let x = (cx * sw - face_x * w as f64).clamp(0.0, sw - w as f64);
    let y = (cy * sh - face_y * h as f64).clamp(0.0, sh - h as f64);
    (even(x).min(even(sw) - w), even(y).min(even(sh) - h), w, h)
}

/// Procura a caixa da webcam ao redor do rosto (lives): as bordas da webcam ficam paradas
/// enquanto o jogo se mexe, então aparecem como linhas fortes na média de vários quadros.
/// Devolve (x0, y0, x1, y1) em frações da imagem.
pub fn detect_cam_box(
    src: &std::path::Path,
    start: f64,
    dur: f64,
    face: (f64, f64, f64),
    sw: f64,
    sh: f64,
) -> Option<(f64, f64, f64, f64)> {
    const AW: usize = 480;
    let ah = ((AW as f64 * sh / sw / 2.0).round() as usize * 2).max(2);
    let n = 12usize;
    let out = face::std_command(paths::ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{start:.3}"),
            "-t",
        ])
        .arg(format!("{dur:.3}"))
        .arg("-i")
        .arg(src)
        .args([
            "-an",
            "-vf",
            &format!(
                "fps={:.4},scale={AW}:{ah},format=gray",
                n as f64 / dur.max(1.0)
            ),
            "-frames:v",
            &n.to_string(),
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let fl = AW * ah;
    let frames: Vec<&[u8]> = out.stdout.chunks_exact(fl).collect();
    if frames.len() < 3 {
        return None;
    }
    // Gradiente médio (horizontal e vertical) de todos os quadros
    let mut gx = vec![0f32; fl];
    let mut gy = vec![0f32; fl];
    for f in &frames {
        for y in 1..ah - 1 {
            for x in 1..AW - 1 {
                let i = y * AW + x;
                gx[i] += (f[i + 1] as f32 - f[i - 1] as f32).abs();
                gy[i] += (f[i + AW] as f32 - f[i - AW] as f32).abs();
            }
        }
    }
    let k = 1.0 / frames.len() as f32;
    for v in gx.iter_mut().chain(gy.iter_mut()) {
        *v *= k;
    }
    // Movimento médio entre quadros: o jogo mexe muito, a webcam pouco
    let mut mv = vec![0f32; fl];
    for pair in frames.windows(2) {
        for (i, m) in mv.iter_mut().enumerate() {
            *m += (pair[1][i] as f32 - pair[0][i] as f32).abs();
        }
    }
    let km = 1.0 / (frames.len() - 1) as f32;
    for m in mv.iter_mut() {
        *m *= km;
    }
    let (fx, fy, fw) = (
        face.0 * AW as f64,
        face.1 * ah as f64,
        (face.2 * AW as f64).max(8.0),
    );
    let ry = (
        (fy - 0.6 * fw).max(1.0) as usize,
        (fy + 0.6 * fw).min(ah as f64 - 2.0) as usize,
    );
    let rx = (
        (fx - 0.6 * fw).max(1.0) as usize,
        (fx + 0.6 * fw).min(AW as f64 - 2.0) as usize,
    );
    let reach = 4.5 * fw;
    // Nota de cada linha candidata: borda parada forte + diferença de movimento entre os lados
    let col_score = |x: usize, outward: i32| -> f64 {
        let (mut edge, mut out_m, mut in_m, mut n) = (0usize, 0f64, 0f64, 0usize);
        for y in ry.0..ry.1 {
            let g = gx[y * AW + x].max(gx[y * AW + (x + 1).min(AW - 1)]);
            if g > 22.0 {
                edge += 1;
            }
            for d in 2..7 {
                let xo = (x as i32 + outward * d).clamp(0, AW as i32 - 1) as usize;
                let xi = (x as i32 - outward * d).clamp(0, AW as i32 - 1) as usize;
                out_m += mv[y * AW + xo] as f64;
                in_m += mv[y * AW + xi] as f64;
            }
            n += 1;
        }
        let n = n.max(1) as f64;
        let edge = edge as f64 / n;
        let contrast = (out_m - in_m) / (n * 5.0);
        if edge > 0.5 && contrast > 4.0 {
            contrast * edge
        } else {
            0.0
        }
    };
    let row_score = |y: usize, outward: i32| -> f64 {
        let (mut edge, mut out_m, mut in_m, mut n) = (0usize, 0f64, 0f64, 0usize);
        for x in rx.0..rx.1 {
            let g = gy[y * AW + x].max(gy[(y + 1).min(ah - 1) * AW + x]);
            if g > 22.0 {
                edge += 1;
            }
            for d in 2..7 {
                let yo = (y as i32 + outward * d).clamp(0, ah as i32 - 1) as usize;
                let yi = (y as i32 - outward * d).clamp(0, ah as i32 - 1) as usize;
                out_m += mv[yo * AW + x] as f64;
                in_m += mv[yi * AW + x] as f64;
            }
            n += 1;
        }
        let n = n.max(1) as f64;
        let edge = edge as f64 / n;
        let contrast = (out_m - in_m) / (n * 5.0);
        if edge > 0.5 && contrast > 4.0 {
            contrast * edge
        } else {
            0.0
        }
    };
    let best = |range: Vec<usize>, score: &dyn Fn(usize) -> f64, edge_default: f64| {
        range
            .into_iter()
            .map(|v| (v, score(v)))
            .filter(|(_, sc)| *sc > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(v, _)| v as f64)
            .unwrap_or(edge_default)
    };
    let x0 = best(
        ((fx - reach).max(1.0) as usize..(fx - 0.25 * fw).max(1.0) as usize).collect(),
        &|x| col_score(x, -1),
        0.0,
    );
    let x1 = best(
        (((fx + 0.25 * fw) as usize).min(AW - 2)..((fx + reach) as usize).min(AW - 2)).collect(),
        &|x| col_score(x, 1),
        AW as f64,
    );
    let y0 = best(
        ((fy - reach).max(1.0) as usize..(fy - 0.25 * fw).max(1.0) as usize).collect(),
        &|y| row_score(y, -1),
        0.0,
    );
    let y1 = best(
        (((fy + 0.25 * fw) as usize).min(ah - 2)..((fy + reach) as usize).min(ah - 2)).collect(),
        &|y| row_score(y, 1),
        ah as f64,
    );
    let (w, h) = (x1 - x0, y1 - y0);
    if std::env::var_os("KERF_DEBUG").is_some() {
        eprintln!(
            "caixa da webcam: rosto ({fx:.0},{fy:.0},{fw:.0}) caixa x {x0}-{x1} y {y0}-{y1} em {AW}x{ah}"
        );
    }
    // Uma caixa de verdade: menor que a tela inteira e maior que o rosto
    if w >= AW as f64 * 0.92 || h >= ah as f64 * 0.92 || w < fw * 0.9 || h < fw * 0.6 {
        return None;
    }
    Some((
        x0 / AW as f64,
        y0 / ah as f64,
        x1 / AW as f64,
        y1 / ah as f64,
    ))
}

/// Maior retângulo com proporção `aspect` dentro da caixa (frações), centrado nela, em pixels.
fn fit_in_box(b: (f64, f64, f64, f64), sw: f64, sh: f64, aspect: f64) -> (u32, u32, u32, u32) {
    let even = |v: f64| ((v / 2.0).floor() as u32).max(1) * 2;
    // Um pouco para dentro, para não pegar a moldura
    let (bx0, by0, bx1, by1) = (
        b.0 * sw + 4.0,
        b.1 * sh + 4.0,
        b.2 * sw - 4.0,
        b.3 * sh - 4.0,
    );
    let (bw, bh) = ((bx1 - bx0).max(16.0), (by1 - by0).max(16.0));
    let (w, h) = if bw / bh > aspect {
        (bh * aspect, bh)
    } else {
        (bw, bw / aspect)
    };
    let x = (bx0 + (bw - w) / 2.0).clamp(0.0, sw - w);
    let y = (by0 + (bh - h) / 2.0).clamp(0.0, sh - h);
    (even(x), even(y), even(w), even(h))
}

/// Retângulo com proporção `aspect` ao redor do rosto (webcam de live), em pixels.
fn cam_rect(face: (f64, f64, f64), sw: f64, sh: f64, aspect: f64) -> (u32, u32, u32, u32) {
    let (cx, cy, fw) = face;
    let even = |v: f64| ((v / 2.0).floor() as u32).max(1) * 2;
    let mut w = (fw * sw * 3.2).clamp(sw * 0.14, sw * 0.7);
    let mut h = w / aspect;
    if h > sh {
        h = sh;
        w = h * aspect;
    }
    let x = (cx * sw - w / 2.0).clamp(0.0, sw - w);
    let y = (cy * sh - h * 0.42).clamp(0.0, sh - h);
    (
        even(x),
        even(y),
        even(w).min(even(sw)),
        even(h).min(even(sh)),
    )
}

// ---------------------------------------------------------------------------
// 4. Montagem
// ---------------------------------------------------------------------------

/// Intervalos (no clipe final) em que alguém está falando, para o React Destaque.
fn talk_spans(words: &[OWord], dur: f64) -> Vec<(f64, f64)> {
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for w in words {
        match spans.last_mut() {
            Some(s) if w.start - s.1 < 0.9 => s.1 = w.end,
            _ => spans.push((w.start, w.end)),
        }
    }
    // Fala muito longa: a webcam fica grande até 5 s e o jogo ganha destaque por 3 s
    let mut out = Vec::new();
    for (a, b) in spans.into_iter().filter(|(a, b)| b - a >= 1.2) {
        let b = (b + 0.25).min(dur);
        let mut t = a;
        while t < b {
            let e = (t + 5.0).min(b);
            // Sobra curta no fim fica junto do último bloco
            let e = if b - e < 1.5 { b } else { e };
            out.push((t, e));
            t = e + 3.0;
        }
    }
    out
}

fn enable_expr(spans: &[(f64, f64)]) -> String {
    if spans.is_empty() {
        return "0".into();
    }
    spans
        .iter()
        .map(|(a, b)| format!("between(t,{a:.3},{b:.3})"))
        .collect::<Vec<_>>()
        .join("+")
}

/// Opção do ffmpeg que lê os filtros de um arquivo: o ffmpeg 7.1+ usa "-/filter_complex"
/// e as versões novas não têm mais "-filter_complex_script".
fn graph_file_flag() -> &'static str {
    static FLAG: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    FLAG.get_or_init(|| {
        let help = face::std_command(paths::ffmpeg())
            .args(["-hide_banner", "-h", "long"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if help.contains("filter_complex_script") {
            "-filter_complex_script"
        } else {
            "-/filter_complex"
        }
    })
}

/// Renderiza um clipe na estrutura pedida e devolve a descrição da edição.
pub async fn render(
    job: &RenderJob<'_>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<String> {
    let cfg: &Config = job.cfg;
    let st = job.structure;
    let opts: StyleOpts = cfg.style(st);
    let (w, h) = cfg.out_size();
    let (wf, hf) = (w as f64, h as f64);
    let fps = render::output_fps(cfg, job.info);
    let (sw, sh) = (job.info.width.max(2) as f64, job.info.height.max(2) as f64);
    let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
    let (rw, rh) = (even(sw), even(sh));
    let raw = matches!(st, "react_split" | "react_destaque" | "meme");

    // Momentos: vários no Top Momentos, um nas outras estruturas
    let moments: Vec<(f64, f64, String)> = if st == "top" && !job.clip.items.is_empty() {
        job.clip
            .items
            .iter()
            .map(|m| (m.start, m.end, m.label.clone()))
            .collect()
    } else {
        vec![(job.start, job.end, String::new())]
    };
    let word_spans: Vec<(f64, f64)> = job
        .transcript
        .words
        .iter()
        .map(|w| (w.start, w.end))
        .collect();
    let (max_gap, pad) = if st == "conversa" {
        (1.1, 0.25)
    } else {
        (0.22, 0.07)
    };
    let mut parts: Vec<Part> = Vec::new();
    for (mi, (a, b, _)) in moments.iter().enumerate() {
        let a = align(*a, fps);
        for mut p in speech_parts(&word_spans, mi, a, *b, opts.cut_pauses, max_gap, pad) {
            p.a = a + align(p.a - a, fps);
            p.b = a + align(p.b - a, fps);
            if p.b - p.a >= 2.0 / fps {
                parts.push(p);
            }
        }
    }
    anyhow::ensure!(!parts.is_empty(), "o trecho escolhido está vazio");

    // Rostos de cada momento
    let need_faces = !matches!(st, "meme");
    let mut faces: Vec<Option<Faces>> = Vec::new();
    for (a, b, _) in &moments {
        if !need_faces {
            faces.push(None);
            continue;
        }
        let small = matches!(st, "react_split" | "react_destaque");
        let f = match face::analyze_opts(job.src, *a, b - a, job.info.width, job.info.height, small)
            .await
        {
            Ok(mut an) => {
                merge_tracks(&mut an);
                let crop_frac = (sh * wf / hf) / sw;
                let plan = face::plan(&an, crop_frac.min(1.0), "speaker");
                Some(Faces {
                    t0: *a,
                    speakers: plan.speakers,
                    a: an,
                })
            }
            Err(e) => {
                eprintln!("análise de rostos falhou: {e:#}");
                None
            }
        };
        faces.push(f);
        if cancel.is_cancelled() {
            anyhow::bail!("Cancelado");
        }
    }
    progress(0.08);

    // Planos
    let rules = shot_rules(st);
    let mut shots: Vec<Shot> = Vec::new();
    for (pi, p) in parts.iter().enumerate() {
        if raw {
            shots.push(Shot {
                part: pi,
                a: p.a,
                b: p.b,
                view: View::Raw,
                reaction: false,
            });
        } else {
            shots.extend(part_shots(
                pi,
                p,
                faces[p.moment].as_ref(),
                &rules,
                opts.reactions && rules.react_min < 90.0,
            ));
        }
    }
    // Planos alinhados a quadros exatos (vídeo e áudio com a mesma duração)
    for s in shots.iter_mut() {
        let p = parts[s.part];
        s.a = p.a + align(s.a - p.a, fps);
        s.b = p.a + align(s.b - p.a, fps);
    }
    shots.retain(|s| s.b - s.a >= 0.5 / fps);
    let abs_words: Vec<(f64, f64, String)> = job
        .transcript
        .words
        .iter()
        .map(|w| (w.start, w.end, w.text.clone()))
        .collect();
    if opts.emphasis_zoom && matches!(st, "turbo" | "cinema") {
        emphasis(&mut shots, &abs_words, rules.base_zoom);
    }

    // Linha do tempo do clipe final: onde cada trecho começa
    let mut out_start: Vec<f64> = Vec::with_capacity(parts.len());
    let mut t = 0.0;
    for (i, p) in parts.iter().enumerate() {
        if i > 0 && st == "top" && p.moment != parts[i - 1].moment {
            t += SEP;
        }
        out_start.push(t);
        t += p.b - p.a;
    }
    let total = t;
    let to_out = |abs: f64| -> Option<f64> {
        parts
            .iter()
            .enumerate()
            .find(|(_, p)| abs >= p.a - 1e-6 && abs < p.b)
            .map(|(i, p)| out_start[i] + (abs - p.a))
    };
    let speaker_of = |abs: f64| -> Option<usize> {
        parts
            .iter()
            .find(|p| abs >= p.a - 0.2 && abs <= p.b + 0.2)
            .and_then(|p| faces[p.moment].as_ref())
            .and_then(|f| f.speaker_at(abs))
        // Pessoas de momentos diferentes não se misturam nas cores
    };
    let owords: Vec<OWord> = job
        .transcript
        .words
        .iter()
        .filter_map(|w| {
            let mid = (w.start + w.end) / 2.0;
            let s = to_out(w.start.max(mid - 0.05)).or_else(|| to_out(mid))?;
            let len = (w.end - w.start).clamp(0.05, 2.0);
            Some(OWord {
                start: s,
                end: (s + len).min(total),
                text: w.text.trim().to_string(),
                speaker: speaker_of(mid),
            })
        })
        .filter(|w| !w.text.is_empty())
        .collect();

    // ------------------------------------------------------------------
    // Filtros do ffmpeg
    // ------------------------------------------------------------------
    let mut g = String::new();
    let n_in = moments.len();
    // Vídeo: um ramo por plano
    for mi in 0..n_in {
        let k = shots.iter().filter(|s| parts[s.part].moment == mi).count();
        if k == 0 {
            continue;
        }
        let labels: String = (0..k).map(|j| format!("[v{mi}_{j}]")).collect();
        let _ = writeln!(g, "[{mi}:v]fps={fps},split={k}{labels};");
    }
    let mut used = vec![0usize; n_in];
    let mut vlabels: Vec<String> = Vec::new();
    let (face_x, face_y) = match st {
        "cinema" => (0.62, 0.40),
        _ => (0.5, 0.36),
    };
    for (si, s) in shots.iter().enumerate() {
        let mi = parts[s.part].moment;
        if si > 0 && st == "top" && parts[shots[si - 1].part].moment != mi {
            let _ = writeln!(
                g,
                "color=c=black:s={w}x{h}:r={fps}:d={SEP},format=yuv420p,setsar=1[sep{si}];"
            );
            vlabels.push(format!("[sep{si}]"));
        }
        let j = used[mi];
        used[mi] += 1;
        let (a, b) = (s.a - moments[mi].0, s.b - moments[mi].0);
        let view = match s.view {
            View::Raw => format!("scale={rw}:{rh},setsar=1,format=yuv420p"),
            View::Crop { cx, cy, zoom } => {
                let (x, y, cw, ch) = crop_rect(cx, cy, zoom, sw, sh, wf, hf, face_x, face_y);
                let sharp = if wf / cw as f64 > 1.1 {
                    ",unsharp=5:5:0.6:5:5:0.0"
                } else {
                    ""
                };
                format!(
                    "crop={cw}:{ch}:{x}:{y},scale={w}:{h}:flags=lanczos{sharp},setsar=1,format=yuv420p"
                )
            }
        };
        let _ = writeln!(
            g,
            "[v{mi}_{j}]trim=start={a:.4}:end={b:.4},setpts=PTS-STARTPTS,{view}[s{si}];"
        );
        vlabels.push(format!("[s{si}]"));
    }
    let _ = writeln!(
        g,
        "{}concat=n={}:v=1:a=0[vcat];",
        vlabels.concat(),
        vlabels.len()
    );
    // Áudio: um ramo por trecho
    let mut alabels: Vec<String> = Vec::new();
    for mi in 0..n_in {
        let k = parts.iter().filter(|p| p.moment == mi).count();
        if k == 0 || !job.info.has_audio {
            continue;
        }
        let labels: String = (0..k).map(|j| format!("[a{mi}_{j}]")).collect();
        let _ = writeln!(g, "[{mi}:a]asplit={k}{labels};");
    }
    let mut aused = vec![0usize; n_in];
    for (pi, p) in parts.iter().enumerate() {
        if pi > 0 && st == "top" && parts[pi - 1].moment != p.moment {
            let _ = writeln!(
                g,
                "anullsrc=r=48000:cl=stereo,atrim=0:{SEP},asetpts=PTS-STARTPTS[q{pi}];"
            );
            alabels.push(format!("[q{pi}]"));
        }
        let len = p.b - p.a;
        if job.info.has_audio {
            let j = aused[p.moment];
            aused[p.moment] += 1;
            let (a, b) = (p.a - moments[p.moment].0, p.b - moments[p.moment].0);
            let fade = 0.012f64.min(len / 4.0);
            let _ = writeln!(
                g,
                "[a{m}_{j}]atrim=start={a:.4}:end={b:.4},asetpts=PTS-STARTPTS,\
aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,\
afade=t=in:d={fade:.3},afade=t=out:st={:.4}:d={fade:.3}[p{pi}];",
                (len - fade).max(0.0),
                m = p.moment
            );
        } else {
            let _ = writeln!(
                g,
                "anullsrc=r=48000:cl=stereo,atrim=0:{len:.4},asetpts=PTS-STARTPTS[p{pi}];"
            );
        }
        alabels.push(format!("[p{pi}]"));
    }
    let _ = writeln!(
        g,
        "{}concat=n={}:v=0:a=1[acat];",
        alabels.concat(),
        alabels.len()
    );

    // Layout e legendas de cada estrutura
    let sc = Scene {
        cfg,
        w,
        h,
        dur: total,
    };
    let words_for_caps: &[OWord] = &owords;
    let title = job.clip.title.trim();
    let mut desc_extra = String::new();
    let main_face = faces
        .first()
        .and_then(|f| f.as_ref())
        .and_then(|f| f.main_face());
    // Caixa da webcam (lives): bordas paradas ao redor do rosto principal
    let cam_box = match (st, main_face) {
        ("react_split" | "react_destaque", Some(face)) => {
            let (src, a, d) = (
                job.src.to_path_buf(),
                moments[0].0,
                moments[0].1 - moments[0].0,
            );
            tokio::task::spawn_blocking(move || detect_cam_box(&src, a, d, face, sw, sh))
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let cam_crop = |aspect: f64| match (cam_box, main_face) {
        (Some(b), _) => Some(fit_in_box(b, rw as f64, rh as f64, aspect)),
        (None, Some(face)) => Some(cam_rect(face, rw as f64, rh as f64, aspect)),
        _ => None,
    };
    let (layout, ass) = match st {
        "react_split" => {
            let top_h = ((hf / 3.0 / 2.0).round() * 2.0) as u32;
            let bot_h = h - top_h;
            let bottom = {
                let (cw, ch) = {
                    let asp = wf / bot_h as f64;
                    let (mut cw, mut ch) = (rh as f64 * asp, rh as f64);
                    if cw > rw as f64 {
                        cw = rw as f64;
                        ch = cw / asp;
                    }
                    (even(cw), even(ch))
                };
                format!(
                    "crop={cw}:{ch}:{}:{},scale={w}:{bot_h}:flags=lanczos",
                    (rw - cw) / 2,
                    (rh - ch) / 2
                )
            };
            let top = match cam_crop(wf / top_h as f64) {
                Some((x, y, cw, ch)) => {
                    desc_extra = "webcam em cima, jogo embaixo".into();
                    format!("crop={cw}:{ch}:{x}:{y},scale={w}:{top_h}:flags=lanczos")
                }
                None => {
                    desc_extra = "sem webcam: vídeo inteiro em cima".into();
                    format!(
                        "scale={w}:{top_h}:force_original_aspect_ratio=decrease:flags=lanczos,\
pad={w}:{top_h}:(ow-iw)/2:(oh-ih)/2:black"
                    )
                }
            };
            let layout = format!(
                "[vcat]split=2[rs_a][rs_b];[rs_a]{top}[rs_t];[rs_b]{bottom}[rs_u];\
[rs_t][rs_u]vstack,setsar=1[vl];"
            );
            let ass = captions::karaoke(
                &sc,
                words_for_caps,
                opts.censor,
                top_h as f64 + bot_h as f64 * 0.42,
                (opts.title && !title.is_empty()).then_some((title, top_h as f64)),
            );
            (layout, ass)
        }
        "react_destaque" => {
            let band_h = even(wf * rh as f64 / rw as f64).min(h / 2);
            let band_y = even(hf * 0.427);
            let cam_w = even(wf * 0.84);
            let cam_h = even(cam_w as f64 * 10.0 / 16.0);
            let cam_y = band_y
                .saturating_sub(cam_h + even(hf * 0.03))
                .max(even(hf * 0.08));
            let pip_w = even(wf * 0.32);
            let pip_h = even(pip_w as f64 * 10.0 / 16.0);
            let spans = talk_spans(words_for_caps, total);
            let e = enable_expr(&spans);
            let base = format!(
                "[vcat]split=3[d_bg][d_g][d_c];\
[d_bg]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},gblur=sigma=38,eq=brightness=-0.16:saturation=0.85[d_b];\
[d_g]scale={w}:{band_h}:flags=lanczos[d_band];\
[d_b][d_band]overlay=0:{band_y}[d_1];"
            );
            let layout = match cam_crop(16.0 / 10.0) {
                Some((x, y, cw, ch)) => {
                    desc_extra = format!("webcam grande em {} momentos", spans.len());
                    format!(
                        "{base}[d_c]crop={cw}:{ch}:{x}:{y},split=2[d_c1][d_c2];\
[d_c1]scale={}:{}:flags=lanczos,pad={cam_w}:{cam_h}:6:6:white[d_big];\
[d_c2]scale={}:{}:flags=lanczos,pad={pip_w}:{pip_h}:4:4:white[d_pip];\
[d_1][d_big]overlay={}:{cam_y}:enable='{e}'[d_2];\
[d_2][d_pip]overlay={}:{}:enable='not({e})',setsar=1[vl];",
                        cam_w - 12,
                        cam_h - 12,
                        pip_w - 8,
                        pip_h - 8,
                        (w - cam_w) / 2,
                        w - pip_w - even(wf * 0.03),
                        band_y + band_h - pip_h - even(wf * 0.025),
                    )
                }
                None => {
                    desc_extra = "sem webcam: jogo em destaque".into();
                    format!("{base}[d_c]nullsink;[d_1]setsar=1[vl];")
                }
            };
            let name_y = cam_y as f64 + cam_h as f64;
            let ass = captions::giant(
                &sc,
                words_for_caps,
                opts.censor,
                band_y as f64 + band_h as f64 + (hf - (band_y + band_h) as f64) * 0.38,
                (opts.title && !title.is_empty()).then_some(title),
                main_face
                    .is_some()
                    .then_some((opts.name_tag.as_str(), name_y, spans.as_slice())),
            );
            (layout, ass)
        }
        "meme" => {
            let band_h = even(wf * rh as f64 / rw as f64).min(even(hf * 0.6));
            let context = if job.clip.context.trim().is_empty() {
                title.to_string()
            } else {
                job.clip.context.trim().to_string()
            };
            let lines: Vec<String> = context
                .to_uppercase()
                .lines()
                .flat_map(|l| captions::wrap(l, 22))
                .take(6)
                .collect();
            let line_h = 70.0 * wf / 1080.0;
            let box_h = even(lines.len().max(1) as f64 * line_h + 80.0 * wf / 1080.0);
            let header = if cfg.watermark_text().is_some() {
                even(110.0 * wf / 1080.0)
            } else {
                0
            };
            let total_h = header + box_h + band_h;
            let y0 = even(((hf - total_h as f64) / 2.0).max(hf * 0.05));
            let box_y = y0 + header;
            let vid_y = box_y + box_h;
            let layout = format!(
                "color=c=black:s={w}x{h}:r={fps}[m_bg];[vcat]scale={w}:{band_h}:flags=lanczos[m_v];\
[m_bg][m_v]overlay=0:{vid_y}:shortest=1,drawbox=x=0:y={box_y}:w={w}:h={box_h}:color=white:t=fill,setsar=1[vl];"
            );
            let ass = captions::meme(
                &sc,
                &lines,
                box_y as f64 + box_h as f64 / 2.0,
                (header > 0).then_some(y0 as f64 + header as f64 / 2.0),
                if opts.captions { words_for_caps } else { &[] },
                vid_y as f64 + band_h as f64 - 24.0 * wf / 1080.0,
            );
            desc_extra = "texto de contexto em cima".into();
            (layout, ass)
        }
        "top" => {
            let items: Vec<(f64, String)> = moments
                .iter()
                .enumerate()
                .filter_map(|(mi, (_, _, label))| {
                    let pi = parts.iter().position(|p| p.moment == mi)?;
                    Some((out_start[pi], label.clone()))
                })
                .collect();
            let list_title = if opts.list_title.trim().is_empty() {
                "Melhores momentos".to_string()
            } else {
                opts.list_title.trim().to_string()
            };
            desc_extra = format!("{} momentos", items.len());
            let ass = captions::top_list(
                &sc,
                &list_title,
                &items,
                if opts.captions { words_for_caps } else { &[] },
            );
            let grade = "eq=contrast=1.05:saturation=1.08";
            (format!("[vcat]{grade},setsar=1[vl];"), ass)
        }
        "cinema" => {
            let grade = if opts.grade {
                "eq=contrast=1.12:saturation=1.12:gamma=0.97,vignette=angle=PI/4.6,unsharp=5:5:0.7:5:5:0.0,"
            } else {
                ""
            };
            (
                format!("[vcat]{grade}setsar=1[vl];"),
                captions::stacked(&sc, words_for_caps, false),
            )
        }
        "conversa" => (
            "[vcat]setsar=1[vl];".into(),
            captions::conversa(&sc, words_for_caps, opts.censor),
        ),
        _ => (
            "[vcat]setsar=1[vl];".into(),
            captions::turbo(&sc, words_for_caps, opts.speaker_colors, opts.censor, 0.30),
        ),
    };
    g.push_str(&layout);
    let ass_name = format!("{}.ass", job.name);
    tokio::fs::write(job.work.join(&ass_name), ass).await?;
    let _ = writeln!(
        g,
        "[vl]ass={ass_name}:fontsdir={}[vout];",
        render::quote_filter_path(&paths::fonts_dir())
    );

    // Música de fundo (abaixa sozinha quando alguém fala) e volume final
    let lufs = structures::loudness(st);
    let music = (!opts.music.trim().is_empty())
        .then(|| paths::music_dir().join(opts.music.trim()))
        .filter(|p| p.is_file());
    match &music {
        Some(_) => {
            let vol = opts.music_volume.clamp(0.0, 1.0);
            let _ = writeln!(
                g,
                "[acat]asplit=2[sp][sc];\
[{n_in}:a]aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo,volume={vol:.3},\
atrim=0:{total:.3},asetpts=PTS-STARTPTS[mu];\
[mu][sc]sidechaincompress=threshold=0.02:ratio=10:attack=15:release=400[duck];\
[sp][duck]amix=inputs=2:duration=first:normalize=0,\
afade=t=out:st={:.3}:d=0.8,loudnorm=I={lufs}:TP=-1.0:LRA=9,aresample=48000[aout]",
                (total - 0.8).max(0.0)
            );
        }
        None => {
            let _ = writeln!(
                g,
                "[acat]loudnorm=I={lufs}:TP=-1.0:LRA=9,aresample=48000[aout]"
            );
        }
    }
    let script = format!("{}.graph", job.name);
    tokio::fs::write(job.work.join(&script), &g).await?;

    // ffmpeg
    let out = format!("{}.mp4", job.name);
    let mut c = proc::command(paths::ffmpeg());
    c.current_dir(job.work);
    c.args(["-hide_banner", "-y", "-nostats", "-progress", "pipe:1"]);
    for (a, b, _) in &moments {
        c.args([
            "-ss",
            &format!("{a:.3}"),
            "-t",
            &format!("{:.3}", b - a + 0.2),
            "-i",
        ])
        .arg(job.src);
    }
    if let Some(m) = &music {
        c.args(["-stream_loop", "-1", "-i"]).arg(m);
    }
    c.args([
        graph_file_flag(),
        &script,
        "-map",
        "[vout]",
        "-map",
        "[aout]",
    ]);
    c.args(render::video_args(cfg, fps));
    c.args([
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ar",
        "48000",
        "-movflags",
        "+faststart",
        "-t",
        &format!("{total:.3}"),
        "-threads",
        &cfg.threads().to_string(),
    ]);
    c.arg(&out);
    proc::run(c, cancel, |line| {
        if let Some(v) = line
            .strip_prefix("out_time_us=")
            .or_else(|| line.strip_prefix("out_time_ms="))
            && let Ok(us) = v.trim().parse::<f64>()
        {
            progress((0.08 + 0.92 * (us / 1e6) / total.max(0.1)).clamp(0.0, 1.0) as f32);
        }
    })
    .await
    .context("falha ao montar o clipe")?;
    render::thumbnail(job.work, job.name, total).await;
    let _ = tokio::fs::remove_file(job.work.join(&script)).await;

    // Descrição para a tela
    let info = structures::info(st).map(|i| i.name).unwrap_or(st);
    let orig: f64 = moments.iter().map(|(a, b, _)| b - a).sum();
    let mut desc = format!("{info} · {} planos", shots.len());
    let reacts = shots.iter().filter(|s| s.reaction).count();
    if reacts > 0 {
        desc.push_str(&format!(" · {reacts} reações"));
    }
    let cut = orig - parts.iter().map(|p| p.b - p.a).sum::<f64>();
    if cut > 0.4 {
        desc.push_str(&format!(" · {:.1} s de pausas removidos", cut).replace('.', ","));
    }
    if !desc_extra.is_empty() {
        desc.push_str(" · ");
        desc.push_str(&desc_extra);
    }
    if music.is_some() {
        desc.push_str(" · com música");
    }
    Ok(desc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_pausas_entre_falas() {
        let words = vec![(0.0, 0.4), (0.45, 0.9), (2.0, 2.4), (2.42, 3.0)];
        let p = speech_parts(&words, 0, 0.0, 4.0, true, 0.22, 0.07);
        assert_eq!(p.len(), 2);
        assert!((p[0].b - 0.97).abs() < 1e-9 && (p[1].a - 1.93).abs() < 1e-9);
        let p = speech_parts(&words, 0, 0.0, 4.0, false, 0.22, 0.07);
        assert_eq!(
            p,
            vec![Part {
                moment: 0,
                a: 0.0,
                b: 4.0
            }]
        );
    }

    #[test]
    fn recorte_vertical_fica_dentro_da_imagem() {
        let (x, y, w, h) = crop_rect(0.95, 0.5, 1.0, 1920.0, 1080.0, 1080.0, 1920.0, 0.5, 0.36);
        assert_eq!((w, h), (606, 1080));
        assert_eq!(x + w, 1920 - 1920 % 2);
        assert_eq!(y, 0);
        let (_, _, w2, _) = crop_rect(0.5, 0.5, 1.3, 1920.0, 1080.0, 1080.0, 1920.0, 0.5, 0.36);
        assert!(w2 < w);
    }

    #[test]
    fn zoom_alterna_no_mesmo_rosto() {
        let v = View::Crop {
            cx: 0.3,
            cy: 0.4,
            zoom: 1.0,
        };
        let mut shots: Vec<Shot> = (0..3)
            .map(|i| Shot {
                part: i,
                a: i as f64 * 3.0,
                b: i as f64 * 3.0 + 3.0,
                view: v,
                reaction: false,
            })
            .collect();
        emphasis(&mut shots, &[], 1.0);
        let z: Vec<f64> = shots
            .iter()
            .map(|s| match s.view {
                View::Crop { zoom, .. } => zoom,
                _ => 0.0,
            })
            .collect();
        assert_eq!(z, vec![1.0, 1.15, 1.0]);
    }

    #[test]
    fn falas_viram_intervalos_de_destaque() {
        let w = |a: f64| OWord {
            start: a,
            end: a + 0.3,
            text: "x".into(),
            speaker: None,
        };
        let spans = talk_spans(&[w(0.0), w(0.5), w(1.0), w(5.0)], 10.0);
        assert_eq!(spans.len(), 1);
        assert_eq!(enable_expr(&spans), "between(t,0.000,1.550)");
    }
}

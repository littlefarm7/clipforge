//! Enquadramento inteligente: encontra as pessoas, descobre quem está falando
//! e escolhe, para cada clipe, a melhor forma de enquadrar.
//!
//! Etapas:
//! 1. `analyze`: lê o trecho a 10 quadros/s, detecta todos os rostos, acompanha cada
//!    pessoa (trilhas), mede o movimento da boca de cada uma, detecta cortes de câmera
//!    do vídeo original e mede o volume da voz.
//! 2. `plan`: decide quem fala a cada instante (Viterbi com penalidade de troca, para não
//!    "piscar" entre pessoas) e escolhe o layout do clipe: seguir o rosto, cortar para
//!    quem fala, enquadrar duas pessoas juntas, tela dividida ou vídeo inteiro.
//! 3. `camera_path`: transforma os alvos em um movimento suave, quadro a quadro, com
//!    cortes secos onde for melhor (troca de pessoa distante ou corte do vídeo original).

use crate::paths;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;

static MODEL: &[u8] = include_bytes!("../assets/seeta_fd_frontal_v1.0.bin");

/// Quadros por segundo da análise.
pub const ANALYSIS_FPS: f64 = 10.0;
/// Largura dos quadros analisados (pixels).
const AW: u32 = 640;
/// Detecta rostos a cada N quadros; entre detecções as pessoas mantêm a posição.
const DETECT_EVERY: usize = 3;
/// Diferença média entre quadros (0–255) acima da qual há um corte de câmera.
const CUT_DIFF: f64 = 28.0;

/// Uma pessoa vista em um quadro.
#[derive(Debug, Clone, Copy)]
pub struct Seen {
    pub track: usize,
    /// Centro horizontal do rosto (0..1 da largura do vídeo)
    pub cx: f64,
    /// Centro vertical do rosto (0..1 da altura do vídeo)
    pub cy: f64,
    /// Largura do rosto (0..1 da largura do vídeo)
    pub w: f64,
    /// Movimento da boca descontado o movimento da cabeça
    pub mouth: f64,
}

#[derive(Debug, Clone, Default)]
pub struct FrameObs {
    pub faces: Vec<Seen>,
    /// Corte de câmera no vídeo original neste quadro
    pub cut: bool,
    /// Volume do áudio (RMS 0..1)
    pub voice: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub frames: Vec<FrameObs>,
    pub tracks: usize,
}

struct Track {
    cx: f64,
    cy: f64,
    w: f64,
    h: f64,
    last_seen: usize,
    ended: bool,
}

fn std_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// Região retangular em pixels: x0..x1, y0..y1.
#[derive(Clone, Copy)]
struct Rect {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

/// Diferença média entre o quadro atual e o anterior deslocado por (dx, dy),
/// amostrando 1 a cada `step` pixels. Pixels que saem da imagem são ignorados.
#[allow(clippy::too_many_arguments)]
fn shifted_diff(
    prev: &[u8],
    cur: &[u8],
    w: usize,
    h: usize,
    r: Rect,
    dx: i32,
    dy: i32,
    step: usize,
) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for y in (r.y0..r.y1).step_by(step) {
        let py = y as i32 + dy;
        if py < 0 || py >= h as i32 {
            continue;
        }
        for x in (r.x0..r.x1).step_by(step) {
            let px = x as i32 + dx;
            if px < 0 || px >= w as i32 {
                continue;
            }
            sum += (cur[y * w + x] as i32 - prev[py as usize * w + px as usize] as i32)
                .unsigned_abs() as u64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum as f64 / n as f64 }
}

/// Encontra o deslocamento do rosto entre dois quadros (balanço da cabeça), testando
/// deslocamentos de até `r` pixels e escolhendo o que melhor encaixa as imagens.
fn head_shift(prev: &[u8], cur: &[u8], w: usize, h: usize, face: Rect, r: i32) -> (i32, i32) {
    let mut best = (f64::MAX, 0, 0);
    for dy in -r..=r {
        for dx in -r..=r {
            let d = shifted_diff(prev, cur, w, h, face, dx, dy, 2);
            if d < best.0 {
                best = (d, dx, dy);
            }
        }
    }
    (best.1, best.2)
}

/// Reduz a imagem pela metade (média de 2×2 pixels), para detectar rostos mais rápido.
fn half_size(img: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = vec![0u8; hw * hh];
    for y in 0..hh {
        for x in 0..hw {
            let i = 2 * y * w + 2 * x;
            out[y * hw + x] =
                ((img[i] as u16 + img[i + 1] as u16 + img[i + w] as u16 + img[i + w + 1] as u16)
                    / 4) as u8;
        }
    }
    out
}

/// Volume da voz por quadro de análise (RMS entre 0 e 1).
fn audio_levels(src: &Path, start: f64, dur: f64, frames: usize) -> Vec<f64> {
    let rate = 8000usize;
    let out = std_command(paths::ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{start:.3}"),
            "-t",
            &format!("{dur:.3}"),
            "-i",
        ])
        .arg(src)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return vec![0.0; frames];
    };
    let samples: Vec<f64> = out
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f64 / 32768.0)
        .collect();
    let per = (rate as f64 / ANALYSIS_FPS) as usize;
    (0..frames)
        .map(|i| {
            let s = &samples[(i * per).min(samples.len())..((i + 1) * per).min(samples.len())];
            if s.is_empty() {
                0.0
            } else {
                (s.iter().map(|v| v * v).sum::<f64>() / s.len() as f64).sqrt()
            }
        })
        .collect()
}

/// Lê o trecho do vídeo e coleta tudo o que é preciso para decidir o enquadramento.
pub async fn analyze(src: &Path, start: f64, dur: f64, src_w: u32, src_h: u32) -> Result<Analysis> {
    let src = src.to_path_buf();
    tokio::task::spawn_blocking(move || analyze_blocking(&src, start, dur, src_w, src_h)).await?
}

fn analyze_blocking(src: &Path, start: f64, dur: f64, src_w: u32, src_h: u32) -> Result<Analysis> {
    let aw = AW as usize;
    let ah =
        ((((AW as f64) * src_h as f64 / src_w.max(1) as f64) / 2.0).round() as usize * 2).max(2);
    let mut child = std_command(paths::ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &format!("{start:.3}"),
            "-t",
            &format!("{dur:.3}"),
            "-i",
        ])
        .arg(src)
        .args([
            "-an",
            "-vf",
            &format!("fps={ANALYSIS_FPS},scale={aw}:{ah},format=gray"),
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("não foi possível ler o vídeo para enquadrar")?;
    let mut stdout = child.stdout.take().context("sem saída do ffmpeg")?;

    let model = rustface::read_model(std::io::Cursor::new(MODEL))?;
    let mut det = rustface::create_detector_with_model(model);
    det.set_min_face_size(20);
    det.set_score_thresh(2.0);
    det.set_pyramid_scale_factor(0.8);
    det.set_slide_window_step(4, 4);

    let frame_len = aw * ah;
    let mut cur = vec![0u8; frame_len];
    let mut prev: Option<Vec<u8>> = None;
    let mut tracks: Vec<Track> = Vec::new();
    let mut frames: Vec<FrameObs> = Vec::new();
    let mut i = 0usize;
    while stdout.read_exact(&mut cur).is_ok() {
        // Corte de câmera: diferença grande entre quadros seguidos (amostrando 1 de 16 pixels)
        let cut = prev.as_ref().is_some_and(|p| {
            let (mut s, mut n) = (0u64, 0u64);
            for k in (0..frame_len).step_by(16) {
                s += (p[k] as i32 - cur[k] as i32).unsigned_abs() as u64;
                n += 1;
            }
            s as f64 / n as f64 > CUT_DIFF
        });
        if cut {
            for t in tracks.iter_mut() {
                t.ended = true;
            }
        }
        let any_active = tracks.iter().any(|t| !t.ended && i - t.last_seen <= 10);
        if i.is_multiple_of(DETECT_EVERY) || cut || !any_active {
            // Detecta na imagem pela metade (4× mais rápido); as posições são relativas (0..1)
            let small = half_size(&cur, aw, ah);
            let (sw, sh) = ((aw / 2) as f64, (ah / 2) as f64);
            let img = rustface::ImageData::new(&small, sw as u32, sh as u32);
            for f in det.detect(&img) {
                let b = f.bbox();
                let (w, h) = (b.width() as f64 / sw, b.height() as f64 / sh);
                let cx = (b.x() as f64 + b.width() as f64 / 2.0) / sw;
                let cy = (b.y() as f64 + b.height() as f64 / 2.0) / sh;
                // Associa à pessoa mais próxima com tamanho parecido
                let best = tracks
                    .iter_mut()
                    .filter(|t| !t.ended && i - t.last_seen <= 20)
                    .filter(|t| {
                        (t.cx - cx).abs() < 0.6 * t.w.max(w) && (t.cy - cy).abs() < 0.6 * t.h.max(h)
                    })
                    .filter(|t| (w / t.w) > 0.6 && (w / t.w) < 1.65)
                    .min_by(|a, b| (a.cx - cx).abs().total_cmp(&(b.cx - cx).abs()));
                match best {
                    Some(t) => {
                        t.cx += (cx - t.cx) * 0.5;
                        t.cy += (cy - t.cy) * 0.5;
                        t.w += (w - t.w) * 0.5;
                        t.h += (h - t.h) * 0.5;
                        t.last_seen = i;
                    }
                    None => tracks.push(Track {
                        cx,
                        cy,
                        w,
                        h,
                        last_seen: i,
                        ended: false,
                    }),
                }
            }
        }
        let mut obs = FrameObs {
            cut,
            ..Default::default()
        };
        for (id, t) in tracks.iter().enumerate() {
            if t.ended || i - t.last_seen > 10 {
                continue;
            }
            // Movimento da boca com o rosto alinhado entre os quadros (cancela o balanço da
            // cabeça). Desconta ainda o que sobra na faixa dos olhos (piscadas, iluminação).
            let mut mouth = 0.0;
            if let (Some(p), false) = (prev.as_ref(), cut) {
                let px = |v: f64| (v * aw as f64).clamp(0.0, aw as f64 - 1.0) as usize;
                let py = |v: f64| (v * ah as f64).clamp(0.0, ah as f64 - 1.0) as usize;
                let top = t.cy - 0.5 * t.h;
                let face = Rect {
                    x0: px(t.cx - 0.5 * t.w),
                    x1: px(t.cx + 0.5 * t.w),
                    y0: py(top),
                    y1: py(top + t.h),
                };
                let reach = ((t.w * aw as f64) * 0.08).round().clamp(2.0, 8.0) as i32;
                let (dx, dy) = head_shift(p, &cur, aw, ah, face, reach);
                let (x0, x1) = (px(t.cx - 0.25 * t.w), px(t.cx + 0.25 * t.w));
                let mouth_r = Rect {
                    x0,
                    x1,
                    y0: py(top + 0.62 * t.h),
                    y1: py(top + 1.0 * t.h),
                };
                let eyes_r = Rect {
                    x0,
                    x1,
                    y0: py(top + 0.22 * t.h),
                    y1: py(top + 0.48 * t.h),
                };
                let m = shifted_diff(p, &cur, aw, ah, mouth_r, dx, dy, 1);
                let e = shifted_diff(p, &cur, aw, ah, eyes_r, dx, dy, 1);
                mouth = (m - e).max(0.0);
            }
            obs.faces.push(Seen {
                track: id,
                cx: t.cx,
                cy: t.cy,
                w: t.w,
                mouth,
            });
        }
        frames.push(obs);
        match prev.as_mut() {
            Some(p) => p.copy_from_slice(&cur),
            None => prev = Some(cur.clone()),
        }
        i += 1;
    }
    let _ = child.wait();
    let voice = audio_levels(src, start, dur, frames.len());
    for (f, v) in frames.iter_mut().zip(voice) {
        f.voice = v;
    }
    Ok(Analysis {
        tracks: tracks.len(),
        frames,
    })
}

/// Posição de uma pessoa na imagem (centro do rosto e largura do rosto, 0..1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    pub x: f64,
    pub y: f64,
    pub w: f64,
}

/// Forma de enquadrar o clipe.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    /// Um alvo por quadro (seguir rosto, quem fala ou duas pessoas juntas)
    Path,
    /// Tela dividida: pessoa de cima e pessoa de baixo
    Split(Spot, Spot),
    /// Vídeo inteiro com fundo desfocado
    Fit,
    /// Recorte central fixo
    Center,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub layout: Layout,
    /// Centro do recorte por quadro de análise (só em `Layout::Path`)
    pub targets: Vec<f64>,
    /// Quadros de análise onde a câmera deve cortar seco
    pub hard_cuts: Vec<usize>,
    /// Descrição curta para mostrar ao usuário
    pub desc: String,
    /// Quem fala em cada quadro de análise (para diagnóstico)
    pub speakers: Vec<Option<usize>>,
    /// Trechos (em segundos) com tela dividida dentro de um clipe que segue quem fala
    pub split_parts: Vec<(f64, f64)>,
    /// Pessoa de cima e pessoa de baixo na tela dividida
    pub split_pos: Option<(Spot, Spot)>,
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Decide quem fala em cada quadro.
///
/// Retorna:
/// - o caminho para a câmera (Viterbi: cada troca custa `switch_cost`, então só troca
///   com evidência firme e não fica "piscando" entre pessoas);
/// - a preferência quadro a quadro, só nos quadros com voz e diferença clara entre as
///   pessoas (usada para medir o ritmo da conversa).
#[allow(clippy::needless_range_loop)]
fn speaker_path(a: &Analysis, main: &[usize]) -> (Vec<Option<usize>>, Vec<Option<usize>>) {
    let n = a.frames.len();
    if n == 0 || main.is_empty() {
        return (vec![None; n], vec![None; n]);
    }
    // Voz: acima do ruído de fundo
    let mut vs: Vec<f64> = a.frames.iter().map(|f| f.voice).collect();
    let floor = {
        let mut c = vs.clone();
        c.sort_by(|x, y| x.total_cmp(y));
        c[c.len() / 5]
    };
    let peak = vs.iter().cloned().fold(0.0, f64::max);
    // Sem nenhum silêncio no trecho, o "ruído de fundo" medido seria a própria voz:
    // o limite nunca passa da metade do pico.
    let thresh = (floor * 2.5).max(peak * 0.08).min(peak * 0.5);
    for v in vs.iter_mut() {
        *v = if *v > thresh { 1.0 } else { 0.0 };
    }
    // Movimento da boca relativo ao "repouso" de cada pessoa (pessoas mais perto da câmera
    // ou mais expressivas mexem mais o rosto mesmo caladas)
    let k = main.len();
    let mut act = vec![vec![f64::NAN; n]; k];
    for (fi, f) in a.frames.iter().enumerate() {
        for s in &f.faces {
            if let Some(j) = main.iter().position(|&m| m == s.track) {
                act[j][fi] = s.mouth;
            }
        }
    }
    let base: Vec<f64> = act
        .iter()
        .map(|row| {
            let mut v: Vec<f64> = row.iter().copied().filter(|x| x.is_finite()).collect();
            if v.is_empty() {
                return 1.0;
            }
            // Nível de repouso: 15% das medições ficam abaixo dele
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() * 15 / 100].max(0.3)
        })
        .collect();
    // Média móvel de ~0,9 s
    let smooth: Vec<Vec<f64>> = act
        .iter()
        .zip(&base)
        .map(|(row, b)| {
            (0..n)
                .map(|i| {
                    let lo = i.saturating_sub(4);
                    let hi = (i + 5).min(n);
                    let vals: Vec<f64> = row[lo..hi]
                        .iter()
                        .copied()
                        .filter(|x| x.is_finite())
                        .collect();
                    if row[i].is_nan() || vals.is_empty() {
                        f64::NAN
                    } else {
                        vals.iter().sum::<f64>() / vals.len() as f64 / b
                    }
                })
                .collect()
        })
        .collect();
    let emission = |j: usize, i: usize| {
        if smooth[j][i].is_nan() {
            -8.0 // pessoa fora da imagem
        } else if vs[i] > 0.5 {
            (0.15 + smooth[j][i]).ln()
        } else {
            0.0 // silêncio: ninguém ganha pontos
        }
    };
    let pref: Vec<Option<usize>> = (0..n)
        .map(|i| {
            if vs[i] < 0.5 {
                return None;
            }
            let mut e: Vec<(f64, usize)> = (0..k).map(|j| (emission(j, i), j)).collect();
            e.sort_by(|a, b| b.0.total_cmp(&a.0));
            (e.len() == 1 || e[0].0 - e[1].0 > 0.3).then_some(main[e[0].1])
        })
        .collect();
    // Viterbi
    let switch_cost = 4.0;
    let mut score = vec![0.0f64; k];
    let mut back = vec![vec![0usize; k]; n];
    for i in 0..n {
        let mut next = vec![f64::NEG_INFINITY; k];
        for j in 0..k {
            let emit = emission(j, i);
            let (mut best, mut arg) = (f64::NEG_INFINITY, j);
            for p in 0..k {
                let s = score[p] - if p == j { 0.0 } else { switch_cost };
                if s > best {
                    best = s;
                    arg = p;
                }
            }
            next[j] = if i == 0 { emit } else { best + emit };
            back[i][j] = arg;
        }
        score = next;
    }
    let mut j = (0..k)
        .max_by(|a, b| score[*a].total_cmp(&score[*b]))
        .unwrap_or(0);
    let mut path = vec![0usize; n];
    for i in (0..n).rev() {
        path[i] = j;
        j = back[i][j];
    }
    // Em silêncio não há como saber o melhor momento da troca: ela fica para quando
    // a próxima pessoa começa a falar (a câmera não "adianta" o corte).
    for i in 1..n {
        if path[i] != path[i - 1] {
            let (old, new) = (path[i - 1], path[i]);
            let mut t = i;
            while t < n && path[t] == new && vs[t] < 0.5 {
                path[t] = old;
                t += 1;
            }
        }
    }
    let path = path
        .into_iter()
        .enumerate()
        .map(|(i, j)| {
            (!smooth[j][i].is_nan() || a.frames[i].faces.iter().any(|s| s.track == main[j]))
                .then_some(main[j])
        })
        .collect();
    (path, pref)
}

/// Encontra os trechos (em quadros) de conversa rápida entre `p1` e `p2`: numa janela de
/// 6 s, pelo menos 12 trocas por minuto e as duas pessoas falando pelo menos 20%.
/// Trechos com menos de 4 s são ignorados; buracos de até 2 s são unidos. As bordas são
/// ajustadas para coincidir com uma troca de quem fala.
fn rapid_parts(
    pref: &[Option<usize>],
    speakers: &[Option<usize>],
    p1: usize,
    p2: usize,
) -> Vec<(usize, usize)> {
    let n = pref.len();
    let half = (3.0 * ANALYSIS_FPS) as usize;
    let mark: Vec<bool> = (0..n)
        .map(|i| {
            let (lo, hi) = (i.saturating_sub(half), (i + half).min(n));
            let win = &pref[lo..hi];
            let voiced = win.iter().flatten().count();
            if voiced < 15 {
                return false;
            }
            let share =
                |p: usize| win.iter().filter(|s| **s == Some(p)).count() as f64 / voiced as f64;
            let minutes = (hi - lo) as f64 / ANALYSIS_FPS / 60.0;
            count_turns(win, 4) as f64 / minutes >= 12.0 && share(p1).min(share(p2)) >= 0.2
        })
        .collect();
    let mut parts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if mark[i] {
            let a = i;
            while i < n && mark[i] {
                i += 1;
            }
            match parts.last_mut() {
                Some(last) if a - last.1 <= (2.0 * ANALYSIS_FPS) as usize => last.1 = i,
                _ => parts.push((a, i)),
            }
        } else {
            i += 1;
        }
    }
    // Ajusta as bordas para a troca de quem fala mais próxima (até 3 s)
    let switch_near = |at: usize| {
        let lo = at.saturating_sub(half);
        let hi = (at + half).min(n.saturating_sub(1));
        (lo.max(1)..=hi)
            .filter(|&k| speakers[k] != speakers[k - 1])
            .min_by_key(|&k| k.abs_diff(at))
    };
    parts
        .into_iter()
        .map(|(a, b)| {
            let a2 = if a == 0 {
                0
            } else {
                switch_near(a).unwrap_or(a)
            };
            let b2 = if b >= n {
                n
            } else {
                switch_near(b).unwrap_or(b)
            };
            (a2, b2.max(a2))
        })
        .filter(|(a, b)| b - a >= (4.0 * ANALYSIS_FPS) as usize)
        .collect()
}

/// Quantas vezes a palavra passa de uma pessoa para outra, desconsiderando
/// trechos de menos de `min_run` quadros com voz (ruído).
fn count_turns(pref: &[Option<usize>], min_run: usize) -> usize {
    let voiced: Vec<usize> = pref.iter().flatten().copied().collect();
    let mut runs: Vec<(usize, usize)> = Vec::new(); // (pessoa, tamanho)
    for p in voiced {
        match runs.last_mut() {
            Some((q, len)) if *q == p => *len += 1,
            _ => runs.push((p, 1)),
        }
    }
    // Junta trechos curtos ao vizinho anterior
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (p, len) in runs {
        match merged.last_mut() {
            Some((q, l)) if *q == p || len < min_run => *l += len,
            _ => merged.push((p, len)),
        }
    }
    merged.len().saturating_sub(1)
}

/// Escolhe o enquadramento do clipe. `crop_frac` é a largura do recorte em relação ao vídeo
/// (ex.: 0,316 para 9:16 a partir de 16:9). `choice` permite ao usuário forçar um modo.
pub fn plan(a: &Analysis, crop_frac: f64, choice: &str) -> Plan {
    let n = a.frames.len();
    let simple = |layout: Layout, desc: &str| Plan {
        layout,
        targets: vec![],
        hard_cuts: vec![],
        desc: desc.into(),
        speakers: vec![],
        split_parts: vec![],
        split_pos: None,
    };
    match choice {
        "center" => return simple(Layout::Center, "Centralizado"),
        "fit" => return simple(Layout::Fit, "Vídeo inteiro"),
        _ => {}
    }
    // Pessoas que aparecem em pelo menos 15% do clipe
    let mut count = vec![0usize; a.tracks];
    for f in &a.frames {
        for s in &f.faces {
            count[s.track] += 1;
        }
    }
    let min_frames = ((n as f64 * 0.15) as usize).max(8);
    let main: Vec<usize> = (0..a.tracks).filter(|&t| count[t] >= min_frames).collect();
    if main.is_empty() {
        return simple(Layout::Fit, "Vídeo inteiro (sem rostos visíveis)");
    }
    let pos = |t: usize| {
        let mut v: Vec<f64> = a
            .frames
            .iter()
            .flat_map(|f| f.faces.iter())
            .filter(|s| s.track == t)
            .map(|s| s.cx)
            .collect();
        median(&mut v)
    };
    let height_pos = |t: usize| {
        let mut v: Vec<f64> = a
            .frames
            .iter()
            .flat_map(|f| f.faces.iter())
            .filter(|s| s.track == t)
            .map(|s| s.cy)
            .collect();
        median(&mut v)
    };
    let width = |t: usize| {
        let mut v: Vec<f64> = a
            .frames
            .iter()
            .flat_map(|f| f.faces.iter())
            .filter(|s| s.track == t)
            .map(|s| s.w)
            .collect();
        median(&mut v)
    };
    let (speakers, pref) = speaker_path(a, &main);
    // Quadros com duas ou mais pessoas principais ao mesmo tempo
    let multi = a
        .frames
        .iter()
        .filter(|f| f.faces.iter().filter(|s| main.contains(&s.track)).count() >= 2)
        .count();
    let multi_frac = multi as f64 / n.max(1) as f64;

    // As duas pessoas que mais aparecem juntas
    let mut by_count = main.clone();
    by_count.sort_by_key(|t| std::cmp::Reverse(count[*t]));
    let duo = (by_count.len() >= 2 && multi_frac >= 0.3).then(|| (by_count[0], by_count[1]));

    let mut split_parts = Vec::new();
    let mut split_pos = None;
    if let Some((p1, p2)) = duo {
        let (x1, x2) = (pos(p1), pos(p2));
        let span = (x1 - x2).abs() + (width(p1) + width(p2)) / 2.0 * 1.4;
        if choice == "auto" && span <= crop_frac * 0.95 {
            // Cabem as duas no quadro: enquadra o grupo
            let mid = (x1 + x2) / 2.0;
            return Plan {
                layout: Layout::Path,
                targets: vec![mid; n],
                hard_cuts: vec![],
                desc: "Duas pessoas juntas no quadro".into(),
                speakers,
                split_parts: vec![],
                split_pos: None,
            };
        }
        // Trechos de conversa rápida (muitas trocas de quem fala em pouco tempo)
        let spot = |t: usize| Spot {
            x: pos(t),
            y: height_pos(t),
            w: width(t),
        };
        let (top, bottom) = if x1 <= x2 {
            (spot(p1), spot(p2))
        } else {
            (spot(p2), spot(p1))
        };
        let rapid = rapid_parts(&pref, &speakers, p1, p2);
        let covered: usize = rapid.iter().map(|(a, b)| b - a).sum();
        if choice == "split" || (choice == "auto" && covered as f64 >= n as f64 * 0.8) {
            // Conversa rápida o clipe todo: tela dividida, quem está à esquerda fica em cima
            return Plan {
                layout: Layout::Split(top, bottom),
                targets: vec![],
                hard_cuts: vec![],
                desc: "Tela dividida (conversa entre duas pessoas)".into(),
                speakers,
                split_parts: vec![],
                split_pos: None,
            };
        }
        if choice == "auto" && !rapid.is_empty() {
            split_parts = rapid
                .iter()
                .map(|(a, b)| (*a as f64 / ANALYSIS_FPS, *b as f64 / ANALYSIS_FPS))
                .collect();
            split_pos = Some((top, bottom));
        }
    }

    // Segue quem fala; cortes secos em trocas distantes e em cortes do vídeo original
    let mut targets = Vec::with_capacity(n);
    let mut hard_cuts: Vec<usize> = Vec::new();
    let mut last_x = pos(by_count[0]);
    let mut last_who: Option<usize> = None;
    let mut switches = 0;
    for (i, f) in a.frames.iter().enumerate() {
        let who = speakers[i].or(last_who);
        let x = who
            .and_then(|t| f.faces.iter().find(|s| s.track == t).map(|s| s.cx))
            .unwrap_or_else(|| who.map(pos).unwrap_or(last_x));
        let last_cut: Option<usize> = hard_cuts
            .last()
            .copied()
            .filter(|&c: &usize| a.frames[c].cut);
        if f.cut {
            hard_cuts.push(i);
        } else if who != last_who && last_who.is_some() {
            switches += 1;
            match last_cut {
                // Troca logo depois de um corte do vídeo original: aplica já no corte
                // (evita dois cortes seguidos)
                Some(c) if i - c <= 10 => {
                    for t in targets.iter_mut().skip(c) {
                        *t = x;
                    }
                }
                _ if (x - last_x).abs() > crop_frac * 0.25 => hard_cuts.push(i),
                _ => {}
            }
        }
        targets.push(x);
        last_x = x;
        last_who = who;
    }
    let mut desc = if main.len() == 1 || switches == 0 {
        "Seguindo o rosto".to_string()
    } else {
        format!(
            "Seguindo quem fala ({switches} {})",
            if switches == 1 { "troca" } else { "trocas" }
        )
    };
    if !split_parts.is_empty() {
        desc.push_str(" + tela dividida na conversa rápida");
    }
    Plan {
        layout: Layout::Path,
        targets,
        hard_cuts,
        desc,
        speakers,
        split_parts,
        split_pos,
    }
}

/// Movimento de câmera suave (uma posição 0..1 por quadro do vídeo final).
///
/// Dentro de cada trecho a câmera usa zona morta + duas molas amortecidas (curva em "S",
/// sem trancos). Nos `hard_cuts` (troca para pessoa distante ou corte do vídeo original)
/// a câmera muda de posição na hora, como um corte de edição.
pub fn camera_path(
    targets: &[f64],
    hard_cuts: &[usize],
    crop_frac: f64,
    dur: f64,
    fps: f64,
) -> Vec<f64> {
    if targets.is_empty() {
        return vec![];
    }
    let step = 1.0 / ANALYSIS_FPS;
    let idx = |t: f64| ((t / step) as usize).min(targets.len() - 1);
    // Zona morta com tempo mínimo (0,5 s) para não reagir a oscilações pequenas
    let dead = crop_frac * 0.12;
    let hold = (0.5 / step).round() as usize;
    let mut goals = Vec::with_capacity(targets.len());
    let mut goal = targets[0];
    let mut outside = 0usize;
    for (i, &t) in targets.iter().enumerate() {
        if hard_cuts.contains(&i) {
            goal = t;
            outside = 0;
        } else if (t - goal).abs() > dead {
            outside += 1;
            if outside >= hold {
                goal = t;
                outside = 0;
            }
        } else {
            outside = 0;
        }
        goals.push(goal);
    }
    let omega = 3.2;
    let lead = 0.4;
    let dt = 1.0 / fps;
    let frames = (dur * fps).ceil() as usize + 1;
    let (mut aim, mut aim_vel) = (goals[0], 0.0);
    let (mut pos, mut vel) = (goals[0], 0.0);
    let mut out = Vec::with_capacity(frames);
    let mut next_cut = 0usize;
    let mut cuts: Vec<usize> = hard_cuts.to_vec();
    cuts.sort_unstable();
    for f in 0..frames {
        let t = f as f64 * dt;
        let i = idx(t);
        // Corte seco: pula direto para o novo alvo
        while next_cut < cuts.len() && cuts[next_cut] <= i {
            let g = goals[cuts[next_cut].min(goals.len() - 1)];
            aim = g;
            pos = g;
            aim_vel = 0.0;
            vel = 0.0;
            next_cut += 1;
        }
        // Olha à frente, mas sem atravessar o próximo corte
        let mut look = idx(t + lead);
        if next_cut < cuts.len() && look >= cuts[next_cut] {
            look = cuts[next_cut].saturating_sub(1).max(i);
        }
        let g = goals[look];
        aim_vel += (omega * omega * (g - aim) - 2.0 * omega * aim_vel) * dt;
        aim += aim_vel * dt;
        vel += (omega * omega * (aim - pos) - 2.0 * omega * vel) * dt;
        pos += vel * dt;
        out.push(pos);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Podcast de um plano só: pessoa 0 à esquerda, pessoa 1 à direita.
    /// `talking(i)` diz quem está falando no quadro i (None = silêncio).
    fn podcast(n: usize, talking: impl Fn(usize) -> Option<usize>) -> Analysis {
        let frames = (0..n)
            .map(|i| {
                let who = talking(i);
                // movimento de boca "de fundo" + forte para quem fala; a pessoa da esquerda
                // está mais perto da câmera (rosto maior, mais movimento natural)
                let noise = |k: usize| ((i * 7 + k * 13) % 5) as f64 * 0.15;
                FrameObs {
                    faces: vec![
                        Seen {
                            track: 0,
                            cx: 0.25,
                            cy: 0.5,
                            w: 0.12,
                            mouth: 1.2 + noise(0) + if who == Some(0) { 3.0 } else { 0.0 },
                        },
                        Seen {
                            track: 1,
                            cx: 0.75,
                            cy: 0.5,
                            w: 0.09,
                            mouth: 0.6 + noise(1) + if who == Some(1) { 2.0 } else { 0.0 },
                        },
                    ],
                    cut: false,
                    voice: if who.is_some() { 0.3 } else { 0.005 },
                }
            })
            .collect();
        Analysis { frames, tracks: 2 }
    }

    #[test]
    fn segue_quem_esta_falando() {
        // 0–6 s fala a pessoa 1 (que tem rosto menor), 6–12 s a pessoa 0
        let a = podcast(120, |i| Some(if i < 60 { 1 } else { 0 }));
        let p = plan(&a, 0.316, "auto");
        assert_eq!(p.layout, Layout::Path);
        assert!((p.targets[30] - 0.75).abs() < 0.01, "começa na pessoa 1");
        assert!((p.targets[100] - 0.25).abs() < 0.01, "termina na pessoa 0");
        assert!(
            p.hard_cuts.iter().any(|&c| (55..=70).contains(&c)),
            "corta na troca: {:?}",
            p.hard_cuts
        );
        assert!(p.desc.contains("quem fala"));
    }

    #[test]
    fn conversa_rapida_vira_tela_dividida() {
        // troca a cada 2 s durante 30 s
        let a = podcast(300, |i| Some((i / 20) % 2));
        let p = plan(&a, 0.316, "auto");
        assert!(matches!(p.layout, Layout::Split(_, _)), "{:?}", p.desc);
    }

    #[test]
    fn pessoas_proximas_ficam_juntas() {
        let mut a = podcast(100, |i| Some((i / 30) % 2));
        for f in a.frames.iter_mut() {
            f.faces[0].cx = 0.45;
            f.faces[1].cx = 0.55;
        }
        let p = plan(&a, 0.316, "auto");
        assert!(p.desc.contains("juntas"));
        assert!((p.targets[0] - 0.5).abs() < 0.01);
    }

    #[test]
    fn sem_rostos_mostra_o_video_inteiro() {
        let a = Analysis {
            frames: vec![FrameObs::default(); 50],
            tracks: 0,
        };
        assert_eq!(plan(&a, 0.316, "auto").layout, Layout::Fit);
    }

    #[test]
    fn camera_suave_e_corte_seco() {
        let mut t = vec![0.25; 50];
        t.extend(vec![0.30; 50]); // pequeno deslocamento: movimento suave
        t.extend(vec![0.75; 50]); // troca de pessoa: corte seco no quadro 100
        let path = camera_path(&t, &[100], 0.316, 15.0, 30.0);
        // antes do corte: sem saltos
        let pre: Vec<f64> = path[..299]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .collect();
        assert!(pre.iter().all(|d| *d < 0.01), "salto antes do corte");
        // no corte: muda na hora
        assert!((path[300] - 0.75).abs() < 0.01 && (path[298] - 0.30).abs() < 0.02);
    }
}

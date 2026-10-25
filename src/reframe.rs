//! Auto reframe: transforma vídeo horizontal em vertical com o rosto de quem fala
//! centralizado, no estilo do Opus Clip.
//!
//! A câmera virtual não segue o rosto em todo quadro (isso treme). Funciona em 2 passos:
//!
//! 1. Análise (`analyze` + `plan`):
//!    - detecta rostos com o YuNet (robusto a perfil) só a cada `key_sec` (keyframes);
//!    - acompanha cada pessoa e mede o movimento da boca dela em todos os quadros
//!      analisados (10 por segundo), com a cabeça estabilizada;
//!    - detecta cortes de cena e divide o clipe em trechos: cada cena, e cada troca de
//!      quem fala, vira um trecho com keyframes e zoom próprios;
//!    - troca de pessoa só quando a outra fala por mais de `switch_sec`;
//!    - limpa os keyframes com mediana de 3 pontos e zona morta.
//! 2. Renderização (`render_video`): a cada quadro, interpola a posição entre os
//!    keyframes (linear), aplica o zoom linear do trecho, recorta, redimensiona e envia
//!    os quadros ao ffmpeg por pipe, junto com o áudio original do mesmo trecho.

use crate::face::{self, ANALYSIS_FPS, CUT_DIFF, Rect};
use crate::proc::ProbeInfo;
use crate::{paths, proc};
use anyhow::{Context, Result, anyhow, bail};
use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use tokio_util::sync::CancellationToken;
use tract_onnx::prelude::*;

/// Detector de rostos YuNet (OpenCV Zoo, licença MIT), embutido no programa.
static YUNET: &[u8] = include_bytes!("../assets/face_detection_yunet_2023mar.onnx");
/// Tamanho da entrada do YuNet (pixels).
const NET: usize = 640;
/// Confiança mínima de uma detecção (0..1).
const MIN_SCORE: f32 = 0.6;
/// Rostos menores que isto (fração da largura) são ignorados.
const MIN_FACE: f64 = 0.025;
/// Duração do zoom rápido nas trocas (s) e quanto ele aproxima.
const PUNCH_SEC: f64 = 0.2;
const PUNCH_ZOOM: f64 = 0.1;

/// Parâmetros do auto reframe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ReframeConfig {
    /// Intervalo entre detecções de rosto (keyframes), em segundos
    pub key_sec: f64,
    /// Zona morta: se o rosto andou menos que isso (fração da largura/altura) desde o
    /// keyframe anterior, a câmera não se move
    pub deadzone: f64,
    /// Zoom no início e no fim de cada trecho (aproximação lenta e constante)
    pub zoom_start: f64,
    pub zoom_end: f64,
    /// Altura do rosto no recorte (0 = topo, 1 = base)
    pub face_y: f64,
    /// Tempo mínimo que outra pessoa precisa falar para a câmera trocar (s)
    pub switch_sec: f64,
    /// Zoom rápido com desfoque de movimento nas trocas de pessoa ou de cena
    pub punch_in: bool,
    /// Tamanho do vídeo final
    pub width: u32,
    pub height: u32,
    /// Quadros por segundo; 0 = os do vídeo original (até 60)
    pub fps: f64,
    pub crf: u32,
    pub preset: String,
    /// Núcleos da CPU para o ffmpeg; 0 = automático
    pub threads: u32,
}

impl Default for ReframeConfig {
    fn default() -> Self {
        Self {
            key_sec: 0.5,
            deadzone: 0.04,
            zoom_start: 1.0,
            zoom_end: 1.12,
            face_y: 0.38,
            switch_sec: 1.0,
            punch_in: false,
            width: 1080,
            height: 1920,
            fps: 0.0,
            crf: 20,
            preset: "veryfast".into(),
            threads: 0,
        }
    }
}

impl ReframeConfig {
    /// Corrige valores fora da faixa (vindos da tela de ajustes ou do arquivo de configuração).
    fn checked(&self) -> Self {
        let mut c = self.clone();
        let num = |v: f64, lo: f64, hi: f64, def: f64| {
            if v.is_finite() { v.clamp(lo, hi) } else { def }
        };
        c.key_sec = num(c.key_sec, 0.1, 5.0, 0.5);
        c.deadzone = num(c.deadzone, 0.0, 0.5, 0.04);
        c.zoom_start = num(c.zoom_start, 1.0, 3.0, 1.0);
        c.zoom_end = num(c.zoom_end, 1.0, 3.0, 1.12);
        c.face_y = num(c.face_y, 0.1, 0.9, 0.38);
        c.switch_sec = num(c.switch_sec, 0.0, 10.0, 1.0);
        c.fps = num(c.fps, 0.0, 120.0, 0.0);
        c.width = (c.width.clamp(64, 4096) / 2) * 2;
        c.height = (c.height.clamp(64, 4096) / 2) * 2;
        c.crf = c.crf.min(51);
        if c.preset.trim().is_empty() {
            c.preset = "veryfast".into();
        }
        c
    }

    /// Keyframes a cada quantos quadros de análise.
    fn key_frames(&self) -> usize {
        ((self.key_sec * ANALYSIS_FPS).round() as usize).max(1)
    }
}

// ---------------------------------------------------------------------------
// Passo 1: análise
// ---------------------------------------------------------------------------

/// Um rosto encontrado pelo detector (coordenadas 0..1 da imagem analisada).
#[derive(Debug, Clone, Copy)]
struct Det {
    cx: f64,
    cy: f64,
    w: f64,
    h: f64,
    score: f32,
    /// Olho direito, olho esquerdo, nariz, canto direito e canto esquerdo da boca
    lm: [(f64, f64); 5],
}

/// Uma pessoa vista em um quadro da análise.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceObs {
    pub track: usize,
    /// Centro do rosto (0..1 da largura e da altura do vídeo)
    pub cx: f64,
    pub cy: f64,
    /// Largura do rosto (0..1 da largura do vídeo)
    pub w: f64,
    /// Movimento da boca, descontado o movimento da cabeça
    pub mouth: f64,
    /// Rosto detectado neste quadro (keyframe); senão é a última posição válida
    pub detected: bool,
}

#[derive(Debug, Clone, Default)]
pub struct FrameObs {
    pub faces: Vec<FaceObs>,
    /// Corte de cena no vídeo original neste quadro
    pub cut: bool,
    /// Volume do áudio (RMS 0..1)
    pub voice: f64,
}

/// Resultado da análise: um registro a cada 0,1 s do clipe.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub frames: Vec<FrameObs>,
    pub tracks: usize,
}

impl Analysis {
    pub fn has_faces(&self) -> bool {
        self.frames.iter().any(|f| !f.faces.is_empty())
    }
}

type Model = Arc<TypedRunnableModel>;

/// Carrega o YuNet uma única vez.
fn yunet() -> Result<&'static Model> {
    static M: OnceLock<std::result::Result<Model, String>> = OnceLock::new();
    M.get_or_init(|| {
        tract_onnx::onnx()
            .model_for_read(&mut std::io::Cursor::new(YUNET))
            .and_then(|m| m.with_input_fact(0, f32::fact([1, 3, NET, NET]).into()))
            .and_then(|m| m.into_optimized())
            .and_then(|m| m.into_runnable())
            .map_err(|e| format!("{e:#}"))
    })
    .as_ref()
    .map_err(|e| anyhow!("detector de rostos indisponível: {e}"))
}

fn iou(a: &Det, b: &Det) -> f64 {
    let ix = ((a.cx + a.w / 2.0).min(b.cx + b.w / 2.0) - (a.cx - a.w / 2.0).max(b.cx - b.w / 2.0))
        .max(0.0);
    let iy = ((a.cy + a.h / 2.0).min(b.cy + b.h / 2.0) - (a.cy - a.h / 2.0).max(b.cy - b.h / 2.0))
        .max(0.0);
    let inter = ix * iy;
    inter / (a.w * a.h + b.w * b.h - inter).max(1e-9)
}

/// Detecta rostos numa imagem BGR de `aw`×`ah` (no máximo 640×640).
fn detect(model: &Model, bgr: &[u8], aw: usize, ah: usize) -> Result<Vec<Det>> {
    // A imagem fica no canto superior esquerdo; o resto da entrada fica preto
    let mut input = tract_ndarray::Array4::<f32>::zeros((1, 3, NET, NET));
    let plane = NET * NET;
    let buf = input.as_slice_mut().context("tensor de entrada")?;
    for y in 0..ah {
        for x in 0..aw {
            let p = (y * aw + x) * 3;
            for c in 0..3 {
                buf[c * plane + y * NET + x] = bgr[p + c] as f32;
            }
        }
    }
    let out = model.run(tvec!(Tensor::from(input).into()))?;
    let mut dets = Vec::new();
    for (si, stride) in [8usize, 16, 32].into_iter().enumerate() {
        let cls = out[si].to_plain_array_view::<f32>()?;
        let obj = out[3 + si].to_plain_array_view::<f32>()?;
        let bbox = out[6 + si].to_plain_array_view::<f32>()?;
        let kps = out[9 + si].to_plain_array_view::<f32>()?;
        let (cls, obj, bbox, kps) = (
            cls.as_slice().context("cls")?,
            obj.as_slice().context("obj")?,
            bbox.as_slice().context("bbox")?,
            kps.as_slice().context("kps")?,
        );
        let cols = NET / stride;
        let s = stride as f64;
        for i in 0..cls.len() {
            let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
            if score < MIN_SCORE {
                continue;
            }
            let (r, c) = ((i / cols) as f64, (i % cols) as f64);
            let b = &bbox[i * 4..i * 4 + 4];
            let cx = (c + b[0] as f64) * s / aw as f64;
            let cy = (r + b[1] as f64) * s / ah as f64;
            let w = (b[2] as f64).exp() * s / aw as f64;
            let h = (b[3] as f64).exp() * s / ah as f64;
            if w < MIN_FACE || !(0.0..=1.0).contains(&cx) || !(0.0..=1.0).contains(&cy) {
                continue;
            }
            let k = &kps[i * 10..i * 10 + 10];
            let mut lm = [(0.0, 0.0); 5];
            for (n, p) in lm.iter_mut().enumerate() {
                *p = (
                    (k[2 * n] as f64 + c) * s / aw as f64,
                    (k[2 * n + 1] as f64 + r) * s / ah as f64,
                );
            }
            dets.push(Det {
                cx,
                cy,
                w,
                h,
                score,
                lm,
            });
        }
    }
    // Remove caixas repetidas do mesmo rosto (NMS)
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Det> = Vec::new();
    for d in dets {
        if keep.iter().all(|k| iou(k, &d) < 0.3) {
            keep.push(d);
        }
    }
    Ok(keep)
}

struct Track {
    d: Det,
    last_seen: usize,
    ended: bool,
}

/// Lê o trecho do vídeo e coleta tudo o que é preciso para planejar a câmera.
pub async fn analyze(
    src: &Path,
    start: f64,
    dur: f64,
    src_w: u32,
    src_h: u32,
    cfg: &ReframeConfig,
) -> Result<Analysis> {
    let src = src.to_path_buf();
    let k = cfg.checked().key_frames();
    tokio::task::spawn_blocking(move || analyze_blocking(&src, start, dur, src_w, src_h, k)).await?
}

fn analyze_blocking(
    src: &Path,
    start: f64,
    dur: f64,
    src_w: u32,
    src_h: u32,
    key_frames: usize,
) -> Result<Analysis> {
    let model = yunet()?;
    // A imagem analisada cabe em 640×640 sem distorcer
    let (sw, sh) = (src_w.max(1) as f64, src_h.max(1) as f64);
    let scale = (NET as f64 / sw).min(NET as f64 / sh);
    let even = |v: f64| (((v / 2.0).floor() as usize) * 2).clamp(2, NET);
    let (aw, ah) = (even(sw * scale), even(sh * scale));
    let mut child = face::std_command(paths::ffmpeg())
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
            "-sn",
            "-vf",
            &format!("fps={ANALYSIS_FPS},scale={aw}:{ah}"),
            "-pix_fmt",
            "bgr24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("não foi possível ler o vídeo para enquadrar")?;
    let mut stdout = child.stdout.take().context("sem saída do ffmpeg")?;

    let mut bgr = vec![0u8; aw * ah * 3];
    let mut cur = vec![0u8; aw * ah];
    let mut prev: Option<Vec<u8>> = None;
    let mut tracks: Vec<Track> = Vec::new();
    let mut frames: Vec<FrameObs> = Vec::new();
    // Uma pessoa sem detecção mantém a última posição válida por até ~1 s
    let hold = 2 * key_frames;
    let mut scene_start = 0usize;
    let mut i = 0usize;
    while stdout.read_exact(&mut bgr).is_ok() {
        for (g, p) in cur.iter_mut().zip(bgr.chunks_exact(3)) {
            *g = ((p[0] as u32 * 29 + p[1] as u32 * 150 + p[2] as u32 * 77) >> 8) as u8;
        }
        // Corte de cena: diferença grande entre quadros seguidos (cenas de pelo menos 0,5 s)
        let cut = i >= scene_start + 5
            && prev.as_ref().is_some_and(|p| {
                let (mut s, mut n) = (0u64, 0u64);
                for k in (0..cur.len()).step_by(16) {
                    s += (p[k] as i32 - cur[k] as i32).unsigned_abs() as u64;
                    n += 1;
                }
                s as f64 / n as f64 > CUT_DIFF
            });
        if cut {
            scene_start = i;
            for t in tracks.iter_mut() {
                t.ended = true;
            }
        }
        // Keyframe: detecta rostos só a cada `key_frames` quadros, contados do início da cena
        let key = (i - scene_start).is_multiple_of(key_frames);
        let mut detected = vec![false; tracks.len()];
        if key {
            let dets = detect(model, &bgr, aw, ah)?;
            // Associa cada rosto à pessoa mais próxima com tamanho parecido (um para um)
            let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
            for (ti, t) in tracks.iter().enumerate() {
                if t.ended || i - t.last_seen > hold {
                    continue;
                }
                for (di, d) in dets.iter().enumerate() {
                    let (dx, dy) = ((t.d.cx - d.cx).abs(), (t.d.cy - d.cy).abs());
                    let ratio = d.w / t.d.w;
                    if dx < t.d.w.max(d.w) && dy < t.d.h.max(d.h) && ratio > 0.6 && ratio < 1.65 {
                        pairs.push((dx + dy, ti, di));
                    }
                }
            }
            pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut used = vec![false; dets.len()];
            for (_, ti, di) in pairs {
                if detected[ti] || used[di] {
                    continue;
                }
                tracks[ti].d = dets[di];
                tracks[ti].last_seen = i;
                detected[ti] = true;
                used[di] = true;
            }
            for (di, d) in dets.iter().enumerate() {
                if !used[di] {
                    tracks.push(Track {
                        d: *d,
                        last_seen: i,
                        ended: false,
                    });
                    detected.push(true);
                }
            }
        }
        let mut obs = FrameObs {
            cut,
            ..Default::default()
        };
        for (id, t) in tracks.iter().enumerate() {
            if t.ended || i - t.last_seen > hold {
                continue;
            }
            let mouth = match (&prev, cut) {
                (Some(p), false) => mouth_motion(p, &cur, aw, ah, &t.d),
                _ => 0.0,
            };
            obs.faces.push(FaceObs {
                track: id,
                cx: t.d.cx,
                cy: t.d.cy,
                w: t.d.w,
                mouth,
                detected: detected[id],
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
    let voice = face::audio_levels(src, start, dur, frames.len());
    for (f, v) in frames.iter_mut().zip(voice) {
        f.voice = v;
    }
    Ok(Analysis {
        tracks: tracks.len(),
        frames,
    })
}

/// Movimento dos lábios entre dois quadros, com o rosto alinhado (cancela o balanço da
/// cabeça) e descontado o que muda na faixa dos olhos (piscadas, luz).
fn mouth_motion(prev: &[u8], cur: &[u8], aw: usize, ah: usize, d: &Det) -> f64 {
    let (fw, fh) = (aw as f64, ah as f64);
    let rect = |x0: f64, x1: f64, y0: f64, y1: f64| {
        let px = |v: f64| (v * fw).clamp(0.0, fw - 1.0) as usize;
        let py = |v: f64| (v * fh).clamp(0.0, fh - 1.0) as usize;
        Rect {
            x0: px(x0),
            x1: px(x1),
            y0: py(y0),
            y1: py(y1),
        }
    };
    let face = rect(
        d.cx - d.w / 2.0,
        d.cx + d.w / 2.0,
        d.cy - d.h / 2.0,
        d.cy + d.h / 2.0,
    );
    // Lábios: entre os cantos da boca, um pouco acima e abaixo da linha da boca
    let (m1, m2) = (d.lm[3], d.lm[4]);
    let mw = (m1.0 - m2.0).abs().max(0.3 * d.w);
    let my = (m1.1 + m2.1) / 2.0;
    let mx0 = m1.0.min(m2.0) - 0.15 * mw;
    let mx1 = m1.0.max(m2.0) + 0.15 * mw;
    // Altura em unidades da altura da imagem: converte a largura da boca
    let mwh = mw * fw / fh;
    let lips = rect(mx0, mx1, my - 0.3 * mwh, my + 0.55 * mwh);
    let (e1, e2) = (d.lm[0], d.lm[1]);
    let ew = (e1.0 - e2.0).abs().max(0.3 * d.w);
    let ey = (e1.1 + e2.1) / 2.0;
    let ewh = ew * fw / fh;
    let eyes = rect(
        e1.0.min(e2.0) - 0.1 * ew,
        e1.0.max(e2.0) + 0.1 * ew,
        ey - 0.25 * ewh,
        ey + 0.25 * ewh,
    );
    if lips.x1 <= lips.x0 + 1 || lips.y1 <= lips.y0 + 1 || face.x1 <= face.x0 + 2 {
        return 0.0;
    }
    let reach = ((d.w * fw) * 0.08).round().clamp(2.0, 8.0) as i32;
    let (dx, dy) = face::head_shift(prev, cur, aw, ah, face, reach);
    let m = face::shifted_diff(prev, cur, aw, ah, lips, dx, dy, 1);
    let e = if eyes.x1 > eyes.x0 + 1 && eyes.y1 > eyes.y0 + 1 {
        face::shifted_diff(prev, cur, aw, ah, eyes, dx, dy, 1)
    } else {
        0.0
    };
    (m - e).max(0.0)
}

// ---------------------------------------------------------------------------
// Passo 1 (continuação): planejamento da câmera
// ---------------------------------------------------------------------------

/// Posição da câmera em um instante (centro do rosto, 0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Key {
    pub t: f64,
    pub cx: f64,
    pub cy: f64,
    /// Largura do rosto (0..1 da largura do vídeo)
    pub w: f64,
}

/// Trecho com keyframes e zoom próprios: uma cena, ou a fala de uma pessoa dentro dela.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    /// Pessoa enquadrada (None = cena sem rostos: recorte central)
    pub track: Option<usize>,
    pub keys: Vec<Key>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub segments: Vec<Segment>,
    /// Trocas de pessoa dentro da mesma cena
    pub switches: usize,
    pub scenes: usize,
    pub desc: String,
}

fn percentile(mut v: Vec<f64>, p: usize) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[(v.len() * p / 100).min(v.len() - 1)]
}

/// Mediana de 3 pontos nos keyframes internos: remove detecções falsas isoladas.
fn median3(keys: &mut [Key]) {
    if keys.len() < 3 {
        return;
    }
    let orig = keys.to_vec();
    let med = |a: f64, b: f64, c: f64| a.max(b).min(a.min(b).max(c));
    for i in 1..keys.len() - 1 {
        keys[i].cx = med(orig[i - 1].cx, orig[i].cx, orig[i + 1].cx);
        keys[i].cy = med(orig[i - 1].cy, orig[i].cy, orig[i + 1].cy);
        keys[i].w = med(orig[i - 1].w, orig[i].w, orig[i + 1].w);
    }
}

/// Zona morta: se o rosto se mexeu menos que `dz` desde o keyframe anterior, a câmera
/// não se move (em cada eixo).
fn deadzone(keys: &mut [Key], dz: f64) {
    let Some(first) = keys.first().copied() else {
        return;
    };
    let (mut x, mut y) = (first.cx, first.cy);
    for k in keys.iter_mut().skip(1) {
        if (k.cx - x).abs() < dz {
            k.cx = x;
        } else {
            x = k.cx;
        }
        if (k.cy - y).abs() < dz {
            k.cy = y;
        } else {
            y = k.cy;
        }
    }
}

/// Centro (x, y) e largura de um rosto.
type Pos = (f64, f64, f64);

/// Decide quem a câmera mostra e gera os trechos com keyframes limpos.
pub fn plan(a: &Analysis, cfg: &ReframeConfig) -> Plan {
    let cfg = cfg.checked();
    let n = a.frames.len();
    let fps = ANALYSIS_FPS;
    let kf = cfg.key_frames();
    let center = |t: f64| Key {
        t,
        cx: 0.5,
        cy: 0.5,
        w: 0.0,
    };
    if n == 0 {
        return Plan {
            segments: vec![],
            switches: 0,
            scenes: 0,
            desc: "Recorte central".into(),
        };
    }

    // Voz acima do ruído de fundo (sem silêncio no trecho, o limite fica na metade do pico)
    let voices: Vec<f64> = a.frames.iter().map(|f| f.voice).collect();
    let floor = percentile(voices.clone(), 20);
    let peak = voices.iter().cloned().fold(0.0, f64::max);
    let thresh = (floor * 2.5).max(peak * 0.08).min(peak * 0.5);
    let voiced: Vec<bool> = voices.iter().map(|v| *v > thresh && *v > 0.0).collect();

    // Repouso da boca de cada pessoa (quem está mais perto da câmera mexe mais o rosto)
    let mut per_track: Vec<Vec<f64>> = vec![Vec::new(); a.tracks];
    for f in a.frames.iter().filter(|f| !f.cut) {
        for s in &f.faces {
            if s.track < a.tracks {
                per_track[s.track].push(s.mouth);
            }
        }
    }
    let rest: Vec<f64> = per_track
        .into_iter()
        .map(|v| percentile(v, 15).max(0.3))
        .collect();

    // Cenas: do início até cada corte
    let mut bounds: Vec<usize> = vec![0];
    bounds.extend((1..n).filter(|&i| a.frames[i].cut));
    bounds.push(n);

    let mut segments: Vec<Segment> = Vec::new();
    let mut switches = 0usize;
    for sc in bounds.windows(2) {
        let (s0, s1) = (sc[0], sc[1]);
        let windows: Vec<(usize, usize)> = (s0..s1)
            .step_by(kf)
            .map(|w0| (w0, (w0 + kf).min(s1)))
            .collect();
        // Quem aparece e quanto mexe a boca (com voz) em cada janela entre keyframes
        let mut vis: Vec<Vec<(usize, f64)>> = Vec::new(); // (pessoa, largura do rosto)
        let mut cand: Vec<Option<usize>> = Vec::new();
        let mut score: Vec<Vec<(usize, f64)>> = Vec::new();
        for &(w0, w1) in &windows {
            let mut v: Vec<(usize, f64)> = Vec::new();
            // Quadros com voz valem mais; nas pausas a boca ainda conta, com peso menor
            let mut sum: Vec<(usize, f64, f64)> = Vec::new(); // (pessoa, soma, pesos)
            for (f, &v_on) in voiced.iter().enumerate().take(w1).skip(w0) {
                let wt = if v_on { 1.0 } else { 0.35 };
                for s in &a.frames[f].faces {
                    if !v.iter().any(|x| x.0 == s.track) {
                        v.push((s.track, s.w));
                    }
                    let r = s.mouth / rest.get(s.track).copied().unwrap_or(1.0);
                    match sum.iter_mut().find(|x| x.0 == s.track) {
                        Some(x) => {
                            x.1 += r * wt;
                            x.2 += wt;
                        }
                        None => sum.push((s.track, r * wt, wt)),
                    }
                }
            }
            let mut sc: Vec<(usize, f64)> = sum
                .iter()
                .filter(|x| x.2 >= 1.0)
                .map(|(t, s, k)| (*t, s / k))
                .collect();
            sc.sort_by(|x, y| y.1.total_cmp(&x.1));
            let c = if v.len() == 1 {
                Some(v[0].0)
            } else if sc.is_empty() {
                None
            } else if sc.len() == 1 || sc[0].1 > sc[1].1 * 1.3 + 0.1 {
                Some(sc[0].0)
            } else {
                None // empate ou silêncio: mantém quem está
            };
            vis.push(v);
            cand.push(c);
            score.push(sc);
        }
        // Começo da cena: quem mais fala nos primeiros ~2 s; sem fala, o maior rosto
        let first_n = ((2.0 * fps) as usize)
            .div_ceil(kf)
            .max(1)
            .min(windows.len());
        let mut totals: Vec<(usize, f64)> = Vec::new();
        for &(t, s) in score.iter().take(first_n).flatten() {
            match totals.iter_mut().find(|x| x.0 == t) {
                Some(x) => x.1 += s,
                None => totals.push((t, s)),
            }
        }
        let largest = |v: &[(usize, f64)]| v.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|x| x.0);
        let init = totals
            .into_iter()
            .filter(|(t, _)| vis[0].iter().any(|x| x.0 == *t))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|x| x.0)
            .or_else(|| largest(&vis[0]));
        // Troca de pessoa só se a outra falar por mais de `switch_sec`; a troca acontece
        // quando ela começou a falar (o clipe é analisado inteiro antes)
        let need = ((cfg.switch_sec * fps) / kf as f64).ceil().max(1.0) as usize;
        let mut who: Vec<Option<usize>> = vec![None; windows.len()];
        let mut cur = init;
        let mut run: Option<(usize, usize, usize)> = None; // (pessoa, janela inicial, janelas)
        for j in 0..windows.len() {
            let visible = |t: usize| vis[j].iter().any(|x| x.0 == t);
            if cur.is_none_or(|c| !visible(c)) {
                // Quem estava sumiu do quadro: vai para quem fala ou para o maior rosto
                let next = cand[j].or_else(|| largest(&vis[j]));
                if j > 0 && next.is_some() && cur.is_some() && next != cur {
                    switches += 1;
                }
                cur = next;
                who[j] = cur;
                run = None;
                continue;
            }
            who[j] = cur;
            match cand[j] {
                Some(c) if Some(c) != cur => {
                    let r = match run {
                        Some((rc, s, k)) if rc == c => (c, s, k + 1),
                        _ => (c, j, 1),
                    };
                    if r.2 >= need {
                        for w in who.iter_mut().take(j + 1).skip(r.1) {
                            *w = Some(c);
                        }
                        cur = Some(c);
                        switches += 1;
                        run = None;
                    } else {
                        run = Some(r);
                    }
                }
                Some(_) => run = None,
                None => {}
            }
        }
        // Trechos: janelas seguidas com a mesma pessoa
        let mut j = 0;
        while j < windows.len() {
            let mut e = j;
            while e + 1 < windows.len() && who[e + 1] == who[j] {
                e += 1;
            }
            let (f0, f1) = (windows[j].0, windows[e].1);
            let (t0, t1) = (f0 as f64 / fps, f1 as f64 / fps);
            let keys = match who[j] {
                None => vec![center(t0), center(t1)],
                Some(track) => {
                    // Keyframe no início de cada janela e no fim do trecho
                    let pos = |f: usize| {
                        a.frames[f]
                            .faces
                            .iter()
                            .find(|s| s.track == track)
                            .map(|s| (s.cx, s.cy, s.w))
                    };
                    let mut ks: Vec<(f64, Option<Pos>)> = windows[j..=e]
                        .iter()
                        .map(|&(w0, _)| (w0 as f64 / fps, pos(w0)))
                        .collect();
                    ks.push((t1, pos(f1 - 1)));
                    // Sem detecção: repete a última posição válida (ou a próxima, no começo)
                    let first = ks.iter().find_map(|k| k.1).unwrap_or((0.5, 0.5, 0.0));
                    let mut last = first;
                    let mut out: Vec<Key> = Vec::with_capacity(ks.len());
                    for (t, p) in ks {
                        if let Some(p) = p {
                            last = p;
                        }
                        out.push(Key {
                            t,
                            cx: last.0,
                            cy: last.1,
                            w: last.2,
                        });
                    }
                    median3(&mut out);
                    deadzone(&mut out, cfg.deadzone);
                    out
                }
            };
            segments.push(Segment {
                start: t0,
                end: t1,
                track: who[j],
                keys,
            });
            j = e + 1;
        }
    }

    let scenes = bounds.len() - 1;
    let faces = segments.iter().any(|s| s.track.is_some());
    let mut desc = if faces {
        "Auto reframe: rosto de quem fala no centro, com zoom suave".to_string()
    } else {
        "Auto reframe: sem rostos, recorte central com zoom suave".to_string()
    };
    if switches > 0 {
        desc.push_str(&format!(
            " · {switches} troca{} de pessoa",
            if switches > 1 { "s" } else { "" }
        ));
    }
    if scenes > 1 {
        desc.push_str(&format!(
            " · {} corte{} de cena",
            scenes - 1,
            if scenes > 2 { "s" } else { "" }
        ));
    }
    Plan {
        segments,
        switches,
        scenes,
        desc,
    }
}

/// Posição da câmera no instante `t`: interpolação linear entre os keyframes.
fn interp(keys: &[Key], t: f64) -> (f64, f64) {
    let Some(first) = keys.first() else {
        return (0.5, 0.5);
    };
    if t <= first.t {
        return (first.cx, first.cy);
    }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if t <= b.t {
            let span = b.t - a.t;
            let p = if span > 1e-9 { (t - a.t) / span } else { 1.0 };
            return (a.cx + (b.cx - a.cx) * p, a.cy + (b.cy - a.cy) * p);
        }
    }
    let l = keys[keys.len() - 1];
    (l.cx, l.cy)
}

/// Zoom linear do quadro `i` de um trecho com `n` quadros.
fn zoom_at(cfg: &ReframeConfig, i: usize, n: usize) -> f64 {
    if n <= 1 {
        return cfg.zoom_start;
    }
    cfg.zoom_start + (cfg.zoom_end - cfg.zoom_start) * i as f64 / (n - 1) as f64
}

/// Recorte (x, y, largura, altura) em pixels de uma imagem `iw`×`ih`.
#[allow(clippy::too_many_arguments)]
fn crop_box(
    cfg: &ReframeConfig,
    iw: f64,
    ih: f64,
    cx: f64,
    cy: f64,
    zoom: f64,
    has_face: bool,
) -> (f64, f64, f64, f64) {
    // Maior retângulo com a proporção da saída que cabe no vídeo
    let aspect = cfg.width as f64 / cfg.height as f64;
    let (bw, bh) = if iw / ih > aspect {
        (ih * aspect, ih)
    } else {
        (iw, iw / aspect)
    };
    let (w, h) = (bw / zoom, bh / zoom);
    let x = (cx * iw - w / 2.0).clamp(0.0, iw - w);
    let y = if has_face {
        cy * ih - cfg.face_y * h
    } else {
        (ih - h) / 2.0
    }
    .clamp(0.0, ih - h);
    (x, y, w, h)
}

// ---------------------------------------------------------------------------
// Passo 2: renderização
// ---------------------------------------------------------------------------

fn spawn_stderr(child: &mut std::process::Child) -> std::thread::JoinHandle<String> {
    let mut err = child.stderr.take();
    std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s.lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// Renderiza o vídeo enquadrado (passo 2), seguindo o plano do passo 1.
#[allow(clippy::too_many_arguments)]
pub async fn render_video(
    src: &Path,
    out: &Path,
    start: f64,
    dur: f64,
    info: &ProbeInfo,
    plan: &Plan,
    cfg: &ReframeConfig,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<()> {
    let cfg = cfg.checked();
    let fps = if cfg.fps > 0.0 {
        cfg.fps
    } else if info.fps > 1.0 {
        info.fps.min(60.0)
    } else {
        30.0
    };
    let done = Arc::new(AtomicU32::new(0));
    let job = {
        let (src, out, plan, cfg) = (
            src.to_path_buf(),
            out.to_path_buf(),
            plan.clone(),
            cfg.clone(),
        );
        let (w, h) = (info.width, info.height);
        let (done, cancel) = (done.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || {
            render_blocking(
                &src,
                &out,
                start,
                dur,
                (w, h),
                fps,
                &plan,
                &cfg,
                &cancel,
                &done,
            )
        })
    };
    tokio::pin!(job);
    let total = (dur * fps).round().max(1.0);
    loop {
        tokio::select! {
            r = &mut job => return r?,
            _ = tokio::time::sleep(std::time::Duration::from_millis(400)) => {
                progress((done.load(Ordering::Relaxed) as f64 / total).min(1.0) as f32);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_blocking(
    src: &Path,
    out: &Path,
    start: f64,
    dur: f64,
    src_size: (u32, u32),
    fps: f64,
    plan: &Plan,
    cfg: &ReframeConfig,
    cancel: &CancellationToken,
    done: &AtomicU32,
) -> Result<()> {
    let (ow, oh) = (cfg.width as usize, cfg.height as usize);
    let (sw, sh) = (src_size.0.max(2) as f64, src_size.1.max(2) as f64);
    // Reduz antes vídeos muito maiores que o necessário (ex.: 4K para 1080×1920)
    let base_h = if sw / sh > ow as f64 / oh as f64 {
        sh
    } else {
        sw * oh as f64 / ow as f64
    };
    let zmax = cfg.zoom_start.max(cfg.zoom_end) * if cfg.punch_in { 1.0 + PUNCH_ZOOM } else { 1.0 };
    let mut s = (oh as f64 * zmax / base_h).min(1.0);
    if s > 0.9 {
        s = 1.0;
    }
    let even = |v: f64| (((v / 2.0).round() as usize) * 2).max(2);
    let (iw, ih) = (even(sw * s), even(sh * s));
    let fps_s = format!("{fps:.6}");

    let mut dec = face::std_command(paths::ffmpeg())
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
            "-sn",
            "-vf",
            &format!("fps={fps_s},scale={iw}:{ih}:flags=lanczos"),
            "-pix_fmt",
            "yuv420p",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("não foi possível ler o vídeo")?;
    let dec_err = spawn_stderr(&mut dec);
    let threads = if cfg.threads > 0 {
        cfg.threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4)
            .clamp(1, 16)
    };
    let mut enc = face::std_command(paths::ffmpeg())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "yuv420p",
            "-s",
            &format!("{ow}x{oh}"),
            "-r",
            &fps_s,
            "-i",
            "pipe:0",
            "-ss",
            &format!("{start:.3}"),
            "-t",
            &format!("{dur:.3}"),
            "-i",
        ])
        .arg(src)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0?",
            "-c:v",
            "libx264",
            "-preset",
            &cfg.preset,
            "-crf",
            &cfg.crf.to_string(),
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-color_range",
            "tv",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-shortest",
            "-movflags",
            "+faststart",
            "-threads",
            &threads.to_string(),
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("não foi possível iniciar o ffmpeg")?;
    let enc_err = spawn_stderr(&mut enc);
    let mut reader = dec.stdout.take().context("sem saída do ffmpeg")?;
    let mut writer = enc.stdin.take().context("sem entrada do ffmpeg")?;

    let (cw, ch) = (iw / 2, ih / 2);
    let mut frame = vec![0u8; iw * ih + 2 * cw * ch];
    let (ocw, och) = (ow / 2, oh / 2);
    let out_len = ow * oh + 2 * ocw * och;
    let mut outbuf = vec![0u8; out_len];
    let mut prevbuf = vec![0u8; out_len];
    let mut mixbuf = vec![0u8; out_len];
    let mut resizer = Resizer::new();
    let luma_alg = ResizeAlg::Convolution(FilterType::Lanczos3);
    let chroma_alg = ResizeAlg::Convolution(FilterType::CatmullRom);

    // Limites de cada trecho em quadros de saída
    let bounds: Vec<(usize, usize)> = plan
        .segments
        .iter()
        .map(|s| {
            (
                (s.start * fps).round() as usize,
                (s.end * fps).round() as usize,
            )
        })
        .collect();
    let mut seg = 0usize;
    let mut j = 0usize;
    let mut result: Result<()> = Ok(());
    while reader.read_exact(&mut frame).is_ok() {
        if cancel.is_cancelled() {
            result = Err(anyhow!("Cancelado"));
            break;
        }
        while seg + 1 < bounds.len() && j >= bounds[seg].1 {
            seg += 1;
        }
        let t = j as f64 / fps;
        let (f0, f1) = bounds.get(seg).copied().unwrap_or((0, usize::MAX));
        let (keys, has_face) = plan
            .segments
            .get(seg)
            .map(|s| (s.keys.as_slice(), s.track.is_some()))
            .unwrap_or((&[], false));
        let i = j.saturating_sub(f0);
        let n = f1.saturating_sub(f0).min((dur * fps).round() as usize + 1);
        let mut zoom = zoom_at(cfg, i, n);
        // Zoom rápido (punch-in) no começo de cada troca de pessoa ou de cena: aproxima
        // em 0,2 s e o zoom lento do trecho continua a partir daí
        let since = t - plan.segments.get(seg).map(|s| s.start).unwrap_or(0.0);
        let punch = cfg.punch_in && seg > 0;
        let moving = punch && since < PUNCH_SEC;
        if punch {
            let p = (since / PUNCH_SEC).clamp(0.0, 1.0);
            zoom *= 1.0 + PUNCH_ZOOM * (1.0 - (1.0 - p) * (1.0 - p));
        }
        let (cx, cy) = interp(keys, t);
        let (x, y, w, h) = crop_box(cfg, iw as f64, ih as f64, cx, cy, zoom, has_face);

        let (sy, rest) = frame.split_at(iw * ih);
        let (su, sv) = rest.split_at(cw * ch);
        let (dy, rest) = outbuf.split_at_mut(ow * oh);
        let (du, dv) = rest.split_at_mut(ocw * och);
        #[allow(clippy::type_complexity)]
        let planes: [(&[u8], usize, usize, &mut [u8], usize, usize, f64, ResizeAlg); 3] = [
            (sy, iw, ih, dy, ow, oh, 1.0, luma_alg),
            (su, cw, ch, du, ocw, och, 0.5, chroma_alg),
            (sv, cw, ch, dv, ocw, och, 0.5, chroma_alg),
        ];
        for (sp, spw, sph, dp, dpw, dph, k, alg) in planes {
            let src_img = ImageRef::new(spw as u32, sph as u32, sp, PixelType::U8)?;
            let mut dst_img = Image::from_slice_u8(dpw as u32, dph as u32, dp, PixelType::U8)?;
            let (cx0, cy0) = (x * k, y * k);
            let (cww, chh) = ((w * k).min(spw as f64 - cx0), (h * k).min(sph as f64 - cy0));
            let opts = ResizeOptions::new()
                .resize_alg(alg)
                .crop(cx0, cy0, cww, chh);
            resizer.resize(&src_img, &mut dst_img, &opts)?;
        }
        // Desfoque de movimento durante o zoom rápido: mistura com o quadro anterior
        let sent = if moving && i > 0 {
            for ((m, o), p) in mixbuf.iter_mut().zip(&outbuf).zip(&prevbuf) {
                *m = (*o as u16 + *p as u16).div_ceil(2) as u8;
            }
            &mixbuf
        } else {
            &outbuf
        };
        if writer.write_all(sent).is_err() {
            result = Err(anyhow!("o ffmpeg parou de receber os quadros"));
            break;
        }
        std::mem::swap(&mut outbuf, &mut prevbuf);
        j += 1;
        done.store(j as u32, Ordering::Relaxed);
    }
    drop(writer);
    drop(reader);
    if result.is_err() {
        let _ = enc.kill();
    }
    let _ = dec.kill();
    let _ = dec.wait();
    let status = enc.wait()?;
    let enc_msg = enc_err.join().unwrap_or_default();
    let dec_msg = dec_err.join().unwrap_or_default();
    result?;
    if j == 0 {
        bail!("não foi possível ler os quadros do vídeo: {dec_msg}");
    }
    if !status.success() {
        bail!("falha ao gerar o vídeo enquadrado ({status}): {enc_msg}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Função principal
// ---------------------------------------------------------------------------

/// Auto reframe completo: analisa o trecho, planeja a câmera e gera `output`
/// (vídeo vertical com o rosto de quem fala no centro e o áudio original).
#[allow(dead_code)] // o app usa `reframe_with`; esta é a forma simples, usada nos testes
pub async fn reframe(
    input: &Path,
    output: &Path,
    start: f64,
    duration: f64,
    cfg: &ReframeConfig,
) -> Result<PathBuf> {
    reframe_with(
        input,
        output,
        start,
        duration,
        cfg,
        &CancellationToken::new(),
        |_| {},
    )
    .await?;
    Ok(output.to_path_buf())
}

/// Igual a `reframe`, com cancelamento e progresso (0..1). Devolve o plano usado.
pub async fn reframe_with(
    input: &Path,
    output: &Path,
    start: f64,
    duration: f64,
    cfg: &ReframeConfig,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<Plan> {
    let info = proc::probe(input).await?;
    if !info.has_video {
        bail!("o arquivo não tem vídeo");
    }
    let a = analyze(input, start, duration, info.width, info.height, cfg).await?;
    let p = plan(&a, cfg);
    progress(0.25);
    render_video(
        input,
        output,
        start,
        duration,
        &info,
        &p,
        cfg,
        cancel,
        |f| progress(0.25 + 0.75 * f),
    )
    .await?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pessoa `track` em (cx, cy) com boca `mouth`.
    fn seen(track: usize, cx: f64, mouth: f64) -> FaceObs {
        FaceObs {
            track,
            cx,
            cy: 0.4,
            w: 0.1,
            mouth,
            detected: true,
        }
    }

    /// Duas pessoas (A em 0.25, B em 0.75); `who(t)` diz quem fala no instante t.
    fn podcast(secs: f64, who: impl Fn(f64) -> Option<usize>) -> Analysis {
        let n = (secs * ANALYSIS_FPS) as usize;
        let frames = (0..n)
            .map(|i| {
                let t = i as f64 / ANALYSIS_FPS;
                let sp = who(t);
                let m = |k: usize| {
                    if sp == Some(k) {
                        3.0 + (i % 3) as f64
                    } else {
                        0.5
                    }
                };
                FrameObs {
                    faces: vec![seen(0, 0.25, m(0)), seen(1, 0.75, m(1))],
                    cut: false,
                    voice: if sp.is_some() { 0.3 } else { 0.001 },
                }
            })
            .collect();
        Analysis { frames, tracks: 2 }
    }

    #[test]
    fn segue_quem_fala_e_troca_na_hora_certa() {
        let a = podcast(8.0, |t| Some(if t < 4.0 { 0 } else { 1 }));
        let p = plan(&a, &ReframeConfig::default());
        assert_eq!(p.segments.len(), 2, "{:?}", p.segments);
        assert_eq!(p.segments[0].track, Some(0));
        assert_eq!(p.segments[1].track, Some(1));
        assert!((p.segments[1].start - 4.0).abs() < 0.01);
        assert!((p.segments[0].keys[0].cx - 0.25).abs() < 1e-9);
        assert!((p.segments[1].keys[0].cx - 0.75).abs() < 1e-9);
        assert_eq!(p.switches, 1);
    }

    #[test]
    fn fala_curta_nao_troca_de_pessoa() {
        // B fala só 0,5 s no meio da fala de A
        let a = podcast(8.0, |t| Some(if (4.0..4.5).contains(&t) { 1 } else { 0 }));
        let p = plan(&a, &ReframeConfig::default());
        assert_eq!(p.segments.len(), 1, "{:?}", p.segments);
        assert_eq!(p.segments[0].track, Some(0));
    }

    #[test]
    fn corte_de_cena_reinicia_o_trecho() {
        let mut a = podcast(6.0, |_| Some(0));
        // Depois do corte, só uma pessoa em outra posição
        for (i, f) in a.frames.iter_mut().enumerate().skip(30) {
            f.cut = i == 30;
            f.faces = vec![seen(2, 0.6, 2.0)];
        }
        a.tracks = 3;
        let p = plan(&a, &ReframeConfig::default());
        assert_eq!(p.scenes, 2);
        assert_eq!(p.segments.len(), 2);
        assert!((p.segments[1].start - 3.0).abs() < 0.01);
        assert_eq!(p.segments[1].track, Some(2));
        // Zoom recomeça no início de cada trecho
        let cfg = ReframeConfig::default();
        assert_eq!(zoom_at(&cfg, 0, 90), 1.0);
        assert!((zoom_at(&cfg, 89, 90) - 1.12).abs() < 1e-9);
        assert!((zoom_at(&cfg, 45, 91) - 1.06).abs() < 1e-9);
    }

    #[test]
    fn mediana_remove_deteccao_falsa_e_zona_morta_segura_a_camera() {
        let k = |t: f64, cx: f64| Key {
            t,
            cx,
            cy: 0.4,
            w: 0.1,
        };
        let mut keys = vec![
            k(0.0, 0.30),
            k(0.5, 0.31),
            k(1.0, 0.90),
            k(1.5, 0.32),
            k(2.0, 0.31),
        ];
        median3(&mut keys);
        assert!(keys.iter().all(|x| x.cx < 0.35), "{keys:?}");
        deadzone(&mut keys, 0.04);
        assert!(keys.iter().all(|x| x.cx == 0.30), "{keys:?}");
        // Movimento maior que a zona morta: a câmera anda em linha reta até lá
        let mut keys = vec![k(0.0, 0.3), k(0.5, 0.4)];
        deadzone(&mut keys, 0.04);
        assert_eq!(keys[1].cx, 0.4);
        let (x, _) = interp(&keys, 0.25);
        assert!((x - 0.35).abs() < 1e-9);
    }

    #[test]
    fn recorte_respeita_as_bordas_e_a_altura_do_rosto() {
        let cfg = ReframeConfig::default();
        // 1920×1080, zoom 1: recorte 607,5×1080, colado na borda esquerda
        let (x, y, w, h) = crop_box(&cfg, 1920.0, 1080.0, 0.0, 0.4, 1.0, true);
        assert_eq!((x, y), (0.0, 0.0));
        assert!((w - 607.5).abs() < 1e-9 && (h - 1080.0).abs() < 1e-9);
        // Com zoom, o rosto fica a 38% da altura do recorte
        let (_, y, _, h) = crop_box(&cfg, 1920.0, 1080.0, 0.5, 0.4, 1.12, true);
        assert!((432.0 - y - 0.38 * h).abs() < 1e-6);
    }

    /// Teste rápido de ponta a ponta: um corte de 6 s sai em 1080×1920 e com áudio.
    #[tokio::test]
    async fn gera_corte_vertical_de_6s_com_audio() {
        let ff = paths::ffmpeg();
        if std::process::Command::new(&ff)
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg não encontrado; teste ignorado");
            return;
        }
        let dir = std::env::temp_dir().join(format!("clipforge-reframe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("entrada.mp4");
        let st = std::process::Command::new(&ff)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1920x1080:rate=30:duration=10",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=10"])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        let out = dir.join("saida.mp4");
        let r = reframe(&src, &out, 2.0, 6.0, &ReframeConfig::default())
            .await
            .unwrap();
        let info = proc::probe(&r).await.unwrap();
        assert_eq!((info.width, info.height), (1080, 1920));
        assert!(
            (info.duration - 6.0).abs() < 0.3,
            "duração {}",
            info.duration
        );
        let streams = std::process::Command::new(paths::ffprobe())
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
            ])
            .arg(&r)
            .output()
            .unwrap();
        let s = String::from_utf8_lossy(&streams.stdout);
        assert!(s.contains("audio"), "sem áudio: {s}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

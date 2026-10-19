//! Rastreamento de rosto para reenquadrar vídeos horizontais em formato vertical.

use crate::paths;
use anyhow::Result;
use std::path::Path;
use tokio::process::Command;

static MODEL: &[u8] = include_bytes!("../assets/seeta_fd_frontal_v1.0.bin");

/// Intervalo entre amostras (segundos).
pub const STEP: f64 = 0.5;

/// Retorna, para cada amostra, o centro horizontal (0..1) do rosto principal.
pub async fn track(src: &Path, start: f64, dur: f64, src_w: u32, src_h: u32) -> Result<Vec<Option<f64>>> {
    let w: u32 = 480;
    let h: u32 = (((w as f64 * src_h as f64 / src_w.max(1) as f64) / 2.0).round() as u32 * 2).max(2);
    let mut c = Command::new(paths::ffmpeg());
    c.args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{start:.3}"), "-t", &format!("{dur:.3}"), "-i"]);
    c.arg(src);
    c.args(["-an", "-vf", &format!("fps={},scale={w}:{h},format=gray", 1.0 / STEP), "-f", "rawvideo", "pipe:1"]);
    let raw = crate::proc::output(c).await?;
    let frame = (w * h) as usize;
    tokio::task::spawn_blocking(move || {
        let model = rustface::read_model(std::io::Cursor::new(MODEL))?;
        let mut det = rustface::create_detector_with_model(model);
        det.set_min_face_size(24);
        det.set_score_thresh(2.0);
        det.set_pyramid_scale_factor(0.8);
        det.set_slide_window_step(4, 4);
        let mut out = Vec::new();
        for f in raw.chunks_exact(frame) {
            let img = rustface::ImageData::new(f, w, h);
            let faces = det.detect(&img);
            let best = faces.iter().max_by(|a, b| {
                let sa = a.bbox().width() as f64 * a.bbox().height() as f64 * a.score().max(0.1);
                let sb = b.bbox().width() as f64 * b.bbox().height() as f64 * b.score().max(0.1);
                sa.total_cmp(&sb)
            });
            out.push(best.map(|b| (b.bbox().x() as f64 + b.bbox().width() as f64 / 2.0) / w as f64));
        }
        Ok(out)
    })
    .await?
}

/// Converte as detecções em um caminho de câmera suave (posição x do recorte por amostra).
/// `crop_frac` é a largura do recorte relativa à imagem.
pub fn camera_path(dets: &[Option<f64>], crop_frac: f64) -> Vec<f64> {
    if dets.is_empty() {
        return vec![];
    }
    // Preenche lacunas com a detecção mais próxima.
    let first = dets.iter().flatten().next().copied().unwrap_or(0.5);
    let mut filled = Vec::with_capacity(dets.len());
    let mut last = first;
    for d in dets {
        if let Some(v) = d {
            last = *v;
        }
        filled.push(last);
    }
    // Mediana móvel para remover falsos positivos.
    let med: Vec<f64> = (0..filled.len())
        .map(|i| {
            let a = i.saturating_sub(2);
            let b = (i + 3).min(filled.len());
            let mut w: Vec<f64> = filled[a..b].to_vec();
            w.sort_by(|x, y| x.total_cmp(y));
            w[w.len() / 2]
        })
        .collect();
    // "Câmera parada": só se move quando o rosto sai da zona morta.
    let dead = crop_frac * 0.18;
    let mut target = med[0];
    let mut pos = med[0];
    let mut out = Vec::with_capacity(med.len());
    for &m in &med {
        if (m - target).abs() > dead {
            target = m;
        }
        pos += (target - pos) * 0.45;
        out.push(pos);
    }
    out
}

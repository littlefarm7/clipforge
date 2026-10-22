//! Rastreamento de rosto para reenquadrar vídeos horizontais em formato vertical.

use crate::paths;
use anyhow::Result;
use std::path::Path;

static MODEL: &[u8] = include_bytes!("../assets/seeta_fd_frontal_v1.0.bin");

/// Intervalo entre amostras da detecção de rosto (segundos).
pub const STEP: f64 = 0.25;

/// Quadros por segundo do movimento da câmera (o mesmo do vídeo final).
pub const FPS: f64 = 30.0;

/// Retorna, para cada amostra, o centro horizontal (0..1) do rosto principal.
pub async fn track(
    src: &Path,
    start: f64,
    dur: f64,
    src_w: u32,
    src_h: u32,
) -> Result<Vec<Option<f64>>> {
    let w: u32 = 480;
    let h: u32 =
        (((w as f64 * src_h as f64 / src_w.max(1) as f64) / 2.0).round() as u32 * 2).max(2);
    let mut c = crate::proc::command(paths::ffmpeg());
    c.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-ss",
        &format!("{start:.3}"),
        "-t",
        &format!("{dur:.3}"),
        "-i",
    ]);
    c.arg(src);
    c.args([
        "-an",
        "-vf",
        &format!("fps={},scale={w}:{h},format=gray", 1.0 / STEP),
        "-f",
        "rawvideo",
        "pipe:1",
    ]);
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
            out.push(
                best.map(|b| (b.bbox().x() as f64 + b.bbox().width() as f64 / 2.0) / w as f64),
            );
        }
        Ok(out)
    })
    .await?
}

/// Converte as detecções em um movimento de câmera suave, com uma posição (0..1) por quadro.
///
/// 1. Lacunas são preenchidas e falsos positivos removidos por mediana.
/// 2. A câmera só escolhe um novo alvo quando o rosto fica fora do centro por um tempo
///    (zona morta + tempo mínimo), para não reagir a detecções passageiras. Como o vídeo
///    já está gravado, o movimento começa um pouco antes, junto com a mudança.
/// 3. A câmera segue o alvo por duas molas amortecidas em sequência: parte do repouso
///    devagar, acelera e freia devagar (curva em "S"), sem trancos nem balanço.
///
/// `crop_frac` é a largura do recorte relativa à imagem; `dur` a duração do clipe.
pub fn camera_path(dets: &[Option<f64>], crop_frac: f64, dur: f64) -> Vec<f64> {
    if dets.is_empty() {
        return vec![];
    }
    let first = dets.iter().flatten().next().copied().unwrap_or(0.5);
    let mut filled = Vec::with_capacity(dets.len());
    let mut last = first;
    for d in dets {
        if let Some(v) = d {
            last = *v;
        }
        filled.push(last);
    }
    // Mediana móvel de ~1,25 s
    let med: Vec<f64> = (0..filled.len())
        .map(|i| {
            let a = i.saturating_sub(2);
            let b = (i + 3).min(filled.len());
            let mut w: Vec<f64> = filled[a..b].to_vec();
            w.sort_by(|x, y| x.total_cmp(y));
            w[w.len() / 2]
        })
        .collect();

    // Alvos com histerese: muda só se o rosto ficar fora da zona morta por HOLD segundos
    let dead = crop_frac * 0.15;
    let hold = (0.5 / STEP).round() as usize;
    let mut targets = Vec::with_capacity(med.len());
    let mut target = med[0];
    let mut outside = 0usize;
    for (i, &m) in med.iter().enumerate() {
        if (m - target).abs() > dead {
            outside += 1;
            if outside >= hold {
                // novo alvo: média das últimas amostras fora da zona
                let a = i + 1 - outside.min(i + 1);
                target = med[a..=i].iter().sum::<f64>() / (i + 1 - a) as f64;
                outside = 0;
            }
        } else {
            outside = 0;
        }
        targets.push(target);
    }

    // Duas molas criticamente amortecidas em cascata, simuladas quadro a quadro.
    // A primeira suaviza o alvo; a segunda move a câmera atrás dele.
    let omega = 3.2; // rapidez: ~1,6 s para completar um giro
    // O vídeo já está gravado: a câmera "olha à frente" e começa a girar junto com a mudança
    let lead = 0.6;
    let dt = 1.0 / FPS;
    let frames = (dur * FPS).ceil() as usize + 1;
    let (mut aim, mut aim_vel) = (targets[0], 0.0);
    let (mut pos, mut vel) = (targets[0], 0.0);
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let t = f as f64 * dt;
        let goal = targets[(((t + lead) / STEP) as usize).min(targets.len() - 1)];
        aim_vel += (omega * omega * (goal - aim) - 2.0 * omega * aim_vel) * dt;
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

    #[test]
    fn camera_se_move_sem_trancos() {
        // Rosto parado à esquerda e, depois de 4 s, à direita (troca de pessoa)
        let dets: Vec<Option<f64>> = (0..40)
            .map(|i| Some(if i < 16 { 0.25 } else { 0.75 }))
            .collect();
        let path = camera_path(&dets, 0.32, 10.0);
        assert!((path[0] - 0.25).abs() < 1e-9);
        assert!(
            (path[path.len() - 1] - 0.75).abs() < 0.01,
            "chega ao destino"
        );
        let speeds: Vec<f64> = path.windows(2).map(|w| (w[1] - w[0]) * FPS).collect();
        // sem passar do alvo (sem balanço)
        assert!(path.iter().all(|&p| p <= 0.7501));
        // aceleração limitada: nada de saltos de um quadro para o outro
        let max_jerk = speeds
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max);
        assert!(
            max_jerk < 0.03,
            "variação de velocidade por quadro {max_jerk}"
        );
    }

    #[test]
    fn deteccao_passageira_nao_move_a_camera() {
        let mut dets: Vec<Option<f64>> = vec![Some(0.5); 40];
        dets[20] = Some(0.9); // um falso positivo
        dets[21] = Some(0.9);
        let path = camera_path(&dets, 0.32, 10.0);
        assert!(path.iter().all(|&p| (p - 0.5).abs() < 1e-6));
    }
}

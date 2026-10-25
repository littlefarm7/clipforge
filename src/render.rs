//! Renderização dos clipes com ffmpeg: corte, reenquadramento e legendas animadas.

use crate::config::Config;
use crate::face;
use crate::paths;
use crate::proc::{self, ProbeInfo};
use crate::reframe::{self, ReframeConfig};
use crate::transcribe::{Transcript, Word};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Parte do progresso do clipe ocupada pelo auto reframe (o resto é o passo das legendas).
const REFRAME_SHARE: f32 = 0.85;

fn ass_color(hex: &str) -> String {
    let h = hex.trim().trim_start_matches('#');
    let (r, g, b) = if h.len() == 6 {
        (&h[0..2], &h[2..4], &h[4..6])
    } else {
        ("FF", "FF", "FF")
    };
    format!(
        "&H00{}{}{}&",
        b.to_uppercase(),
        g.to_uppercase(),
        r.to_uppercase()
    )
}

fn ass_time(t: f64) -> String {
    let t = t.max(0.0);
    let cs = (t * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360000,
        (cs / 6000) % 60,
        (cs / 100) % 60,
        cs % 100
    )
}

fn ass_escape(s: &str) -> String {
    s.replace('\\', "＼").replace('{', "(").replace('}', ")")
}

/// Gera legendas estilo "karaokê": grupos curtos com a palavra atual destacada.
/// `position` substitui a posição configurada (a tela dividida usa o meio, na emenda).
/// Nos trechos de `split_parts` (segundos do clipe) as legendas também vão para o meio.
#[allow(clippy::too_many_arguments)]
pub fn build_ass(
    cfg: &Config,
    words: &[Word],
    start: f64,
    end: f64,
    w: u32,
    h: u32,
    position: Option<&str>,
    split_parts: &[(f64, f64)],
) -> String {
    let scale = w.min(h) as f64 / 1080.0;
    let size = (cfg.sub_size as f64 * scale).round();
    let vertical = h > w;
    let (align, margin_v) = match position.unwrap_or(cfg.sub_position.as_str()) {
        "middle" => (5, 0.0),
        "top" => (8, h as f64 * 0.12),
        _ => (2, h as f64 * if vertical { 0.20 } else { 0.10 }),
    };
    let (font, bold) = match cfg.sub_font.as_str() {
        "Poppins" => ("Poppins", -1),
        "Anton" => ("Anton", 0),
        other => (other, -1),
    };
    let outline = (size * 0.08).max(2.0).round();
    let mut s = String::new();
    let _ = write!(
        s,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
Style: Default,{font},{size},{primary},&H000000FF&,&H00000000&,&H96000000&,{bold},0,0,0,100,100,1,0,1,{outline},{shadow},{align},{ml},{ml},{mv},1\n\n\
[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n",
        primary = ass_color(&cfg.sub_color),
        shadow = (outline * 0.6).round(),
        ml = (w as f64 * 0.08).round(),
        mv = margin_v.round(),
    );
    let hl = ass_color(&cfg.sub_highlight);
    let words: Vec<Word> = words
        .iter()
        .filter(|x| x.end > start && x.start < end)
        .map(|x| Word {
            start: (x.start - start).max(0.0),
            end: (x.end - start).min(end - start),
            text: if cfg.sub_uppercase {
                x.text.to_uppercase()
            } else {
                x.text.clone()
            },
        })
        .collect();
    // Agrupa palavras em linhas curtas
    let per = cfg.words_per_line.clamp(1, 8) as usize;
    let mut groups: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    for (i, wd) in words.iter().enumerate() {
        let gap = words.get(i + 1).map(|n| n.start - wd.end).unwrap_or(99.0);
        let punct = wd.text.ends_with(['.', '?', '!', ',', ';', ':', '…']);
        cur.push(wd.clone());
        let chars: usize = cur.iter().map(|x| x.text.chars().count() + 1).sum();
        if cur.len() >= per || punct || gap > 0.6 || chars > 22 {
            groups.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    for (gi, g) in groups.iter().enumerate() {
        let next_start = groups.get(gi + 1).map(|n| n[0].start).unwrap_or(f64::MAX);
        let g_end = (g.last().unwrap().end + 0.25).min(next_start);
        for (i, wd) in g.iter().enumerate() {
            let t0 = if i == 0 {
                wd.start
            } else {
                wd.start.max(g[i - 1].start)
            };
            let t1 = if i + 1 < g.len() {
                g[i + 1].start
            } else {
                g_end
            };
            if t1 - t0 < 0.01 {
                continue;
            }
            let text = g
                .iter()
                .enumerate()
                .map(|(j, x)| {
                    let t = ass_escape(x.text.trim_end_matches([',', ';']));
                    if j == i {
                        format!("{{\\c{hl}\\fscx108\\fscy108}}{t}{{\\r}}")
                    } else {
                        t
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            // Durante a tela dividida, a legenda fica no meio (na emenda das duas imagens)
            let mid = if split_parts.iter().any(|(a, b)| t0 >= *a && t0 < *b) {
                "{\\an5}"
            } else {
                ""
            };
            let _ = writeln!(
                s,
                "Dialogue: 0,{},{},Default,,0,0,0,,{mid}{}",
                ass_time(t0),
                ass_time(t1),
                text
            );
        }
    }
    s
}

fn quote_filter_path(p: &Path) -> String {
    // O ffmpeg aceita "/" no Windows; ":" (de "C:") precisa ser escapado no filtro
    let s = p.to_string_lossy().replace('\\', "/");
    format!("'{}'", s.replace('\'', "'\\''").replace(':', "\\:"))
}

pub struct RenderJob<'a> {
    pub cfg: &'a Config,
    pub src: &'a Path,
    pub info: &'a ProbeInfo,
    pub transcript: &'a Transcript,
    pub work: &'a Path,
    pub name: &'a str,
    pub start: f64,
    pub end: f64,
    /// Enquadramento escolhido para o clipe: "auto", "speaker", "split", "center" ou "fit"
    pub framing: &'a str,
}

/// Filtros da tela dividida: duas metades empilhadas, cada uma aproximada no rosto de
/// uma pessoa (o rosto ocupa cerca de um terço da largura). `input` é o rótulo de
/// entrada e `out` o de saída.
fn split_chain(
    input: &str,
    out: &str,
    top: face::Spot,
    bottom: face::Spot,
    src: (f64, f64),
    w: u32,
    h: u32,
) -> String {
    let (sw0, sh0) = src;
    let ph = (h / 4) * 2;
    let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
    let panel = |p: face::Spot| {
        // Largura do recorte: rosto ≈ 32% da largura do painel, sem passar da imagem
        let mut cw = (p.w * sw0 / 0.32).max(w as f64 * 0.5);
        let mut ch = cw * ph as f64 / w as f64;
        if ch > sh0 {
            ch = sh0;
            cw = ch * w as f64 / ph as f64;
        }
        if cw > sw0 {
            cw = sw0;
            ch = cw * ph as f64 / w as f64;
        }
        let (cw, ch) = (even(cw).min(even(sw0) - 2), even(ch).min(even(sh0) - 2));
        // Rosto um pouco acima do centro do painel
        let x = ((p.x * sw0 - cw as f64 / 2.0).clamp(0.0, sw0 - cw as f64) as u32 / 2) * 2;
        let y = ((p.y * sh0 - ch as f64 * 0.42).clamp(0.0, sh0 - ch as f64) as u32 / 2) * 2;
        let sharp = if w as f64 / cw as f64 > 1.1 {
            ",unsharp=5:5:0.6:5:5:0.0"
        } else {
            ""
        };
        format!("crop={cw}:{ch}:{x}:{y},scale={w}:{ph}:flags=lanczos{sharp}")
    };
    format!(
        "[{input}]split=2[sa][sb];[sa]{}[st];[sb]{}[su];[st][su]vstack,scale={w}:{h},setsar=1[{out}]",
        panel(top),
        panel(bottom)
    )
}

/// Renderiza o clipe e devolve a descrição do enquadramento usado.
pub async fn render(
    job: RenderJob<'_>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<String> {
    let cfg = job.cfg;
    let (w, h) = cfg.out_size();
    let dur = (job.end - job.start).max(0.5);
    // Mantém a fluidez do original: 60 quadros/s quando o vídeo tiver, senão 30
    let fps: f64 = if cfg.quality != "720" && job.info.fps >= 47.0 {
        60.0
    } else {
        30.0
    };
    let mut graph;
    let mut inputs: Vec<String> = vec![];
    let mut framing_desc = String::from("Só áudio");
    let mut sub_position: Option<&str> = None;
    let mut split_parts: Vec<(f64, f64)> = Vec::new();
    // Vídeo já enquadrado pelo auto reframe (entrada do passo das legendas)
    let mut reframed: Option<std::path::PathBuf> = None;

    if !job.info.has_video {
        // Só áudio (podcast): fundo escuro + legendas
        inputs.extend([
            "-f".into(),
            "lavfi".into(),
            "-t".into(),
            format!("{dur:.3}"),
            "-i".into(),
            format!("color=c=0x111318:s={w}x{h}:r={fps}"),
        ]);
        graph = "[1:v]setsar=1[v0]".to_string();
    } else {
        let (sw0, sh0) = (job.info.width.max(1) as f64, job.info.height.max(1) as f64);
        // Largura da imagem escalada para a altura de saída
        let sw = ((sw0 * h as f64 / sh0 / 2.0).round() as u32) * 2;
        let mut mode = cfg.crop_mode.as_str();
        if mode == "face" && sw <= w + 8 {
            mode = "center";
        }
        // Quanto a imagem é ampliada: acima de 10% recebe um leve realce de nitidez
        let upscale = if mode == "blur" {
            (w as f64 / sw0).min(h as f64 / sh0)
        } else {
            (w as f64 / sw0).max(h as f64 / sh0)
        };
        let sharpen = if upscale > 1.1 {
            ",unsharp=5:5:0.6:5:5:0.0"
        } else {
            ""
        };
        // Escolha do enquadramento: a do clipe (se houver) ou a geral
        let mut choice = match job.framing {
            "" | "auto" => match mode {
                "center" => "center",
                "blur" => "fit",
                _ => "auto",
            },
            c => c,
        };
        let crop_frac = w as f64 / sw as f64;
        // Automático: auto reframe (câmera virtual com keyframes e zoom linear). Gera um
        // vídeo intermediário já enquadrado; as legendas entram no passo seguinte.
        if choice == "auto" && sw > w + 8 {
            let rc = ReframeConfig {
                width: w,
                height: h,
                fps,
                // Intermediário quase sem perdas: o vídeo é comprimido de novo com as legendas
                crf: 12,
                preset: "veryfast".into(),
                threads: cfg.threads(),
                ..cfg.reframe.clone()
            };
            match reframe::analyze(
                job.src,
                job.start,
                dur,
                job.info.width,
                job.info.height,
                &rc,
            )
            .await
            {
                Ok(a) if a.has_faces() => {
                    let p = reframe::plan(&a, &rc);
                    let tmp = job.work.join(format!("{}.reframe.mp4", job.name));
                    let r = reframe::render_video(
                        job.src,
                        &tmp,
                        job.start,
                        dur,
                        job.info,
                        &p,
                        &rc,
                        cancel,
                        |f| progress(f * REFRAME_SHARE),
                    )
                    .await;
                    match r {
                        Ok(()) => {
                            framing_desc = p.desc;
                            reframed = Some(tmp);
                        }
                        Err(e) if cancel.is_cancelled() => return Err(e),
                        Err(e) => {
                            eprintln!("auto reframe falhou: {e:#}");
                            choice = "speaker";
                        }
                    }
                }
                // Sem ninguém na imagem: mostra o vídeo inteiro
                Ok(_) => choice = "fit",
                Err(e) => {
                    eprintln!("auto reframe falhou: {e:#}");
                    choice = "speaker";
                }
            }
        }
        let plan = if reframed.is_some() {
            face::plan(&face::Analysis::default(), crop_frac, "center")
        } else if sw <= w + 8 && choice != "fit" {
            // Vídeo original já vertical: não há o que recortar dos lados
            face::plan(&face::Analysis::default(), crop_frac, "center")
        } else if choice == "center" || choice == "fit" {
            face::plan(&face::Analysis::default(), crop_frac, choice)
        } else {
            match face::analyze(job.src, job.start, dur, job.info.width, job.info.height).await {
                Ok(a) => face::plan(&a, crop_frac, choice),
                Err(e) => {
                    eprintln!("análise de enquadramento falhou: {e:#}");
                    face::plan(&face::Analysis::default(), crop_frac, "center")
                }
            }
        };
        if reframed.is_none() {
            framing_desc = plan.desc.clone();
        }
        graph = match plan.layout {
            _ if reframed.is_some() => "[0:v]setsar=1[v0]".to_string(),
            face::Layout::Fit => format!(
                "[0:v]split=2[a][b];[a]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},boxblur=20:2[bg];\
[b]scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos{sharpen}[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2,setsar=1[v0]"
            ),
            face::Layout::Split(top, bottom) => {
                sub_position = Some("middle");
                split_chain("0:v", "v0", top, bottom, (sw0, sh0), w, h)
            }
            face::Layout::Path => {
                let path = face::camera_path(&plan.targets, &plan.hard_cuts, crop_frac, dur, fps);
                let max_x = (sw - w) as f64;
                let to_x = |c: f64| (c * sw as f64 - w as f64 / 2.0).clamp(0.0, max_x).round();
                let x0 = path
                    .first()
                    .map(|c| to_x(*c))
                    .unwrap_or((max_x / 2.0).round());
                // Uma posição por quadro: movimento contínuo (ou corte seco quando planejado)
                let mut cmds = String::new();
                let mut prev = x0;
                for (k, c) in path.iter().enumerate() {
                    let x = to_x(*c);
                    if x != prev {
                        let _ = writeln!(cmds, "{:.4} crop@cf x {x};", k as f64 / fps);
                        prev = x;
                    }
                }
                let cmd_name = format!("{}.cmd", job.name);
                tokio::fs::write(job.work.join(&cmd_name), &cmds).await?;
                let send = if cmds.is_empty() {
                    String::new()
                } else {
                    format!("sendcmd=f={cmd_name},")
                };
                match plan.split_pos.filter(|_| !plan.split_parts.is_empty()) {
                    // Clipe misto: segue quem fala e mostra a tela dividida só na conversa rápida
                    Some((top, bottom)) => {
                        split_parts = plan.split_parts.clone();
                        let enable: Vec<String> = plan
                            .split_parts
                            .iter()
                            .map(|(a, b)| format!("between(t\\,{a:.2}\\,{b:.2})"))
                            .collect();
                        format!(
                            "[0:v]split=2[pa][pb];[pa]scale={sw}:{h}:flags=lanczos,{send}crop@cf={w}:{h}:{x0}:0{sharpen},setsar=1[pv];\
{};[pv][sv]overlay=0:0:enable='{}'[v0]",
                            split_chain("pb", "sv", top, bottom, (sw0, sh0), w, h),
                            enable.join("+")
                        )
                    }
                    None => format!(
                        "[0:v]scale={sw}:{h}:flags=lanczos,{send}crop@cf={w}:{h}:{x0}:0{sharpen},setsar=1[v0]"
                    ),
                }
            }
            face::Layout::Center => format!(
                "[0:v]scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,crop={w}:{h}{sharpen},setsar=1[v0]"
            ),
        };
    }

    if cfg.subtitles && !job.transcript.words.is_empty() {
        let ass_name = format!("{}.ass", job.name);
        let ass = build_ass(
            cfg,
            &job.transcript.words,
            job.start,
            job.end,
            w,
            h,
            sub_position,
            &split_parts,
        );
        tokio::fs::write(job.work.join(&ass_name), ass).await?;
        let _ = write!(
            graph,
            ";[v0]ass={ass_name}:fontsdir={}[v]",
            quote_filter_path(&paths::fonts_dir())
        );
    } else {
        graph.push_str(";[v0]null[v]");
    }

    let out = format!("{}.mp4", job.name);
    let mut c = crate::proc::command(paths::ffmpeg());
    c.current_dir(job.work);
    c.args(["-hide_banner", "-y", "-nostats", "-progress", "pipe:1"]);
    match &reframed {
        Some(v) => {
            c.arg("-i").arg(v);
        }
        None => {
            c.args([
                "-ss",
                &format!("{:.3}", job.start),
                "-t",
                &format!("{dur:.3}"),
                "-i",
            ])
            .arg(job.src);
        }
    }
    c.args(&inputs);
    c.args(["-filter_complex", &graph, "-map", "[v]", "-map", "0:a?"]);
    let fps_s = fps.to_string();
    let gop = (fps * 2.0).to_string();
    if cfg.quality == "720" {
        // "Mais rápida": prioriza o tempo de renderização
        c.args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "21"]);
    } else {
        // Máxima nitidez: compressão cuidadosa, qualidade visual próxima do original
        c.args(["-c:v", "libx264", "-preset", "medium", "-crf", "17"]);
        c.args([
            "-profile:v",
            "high",
            "-level:v",
            "4.2",
            "-bf",
            "3",
            "-aq-mode",
            "3",
        ]);
    }
    c.args(["-pix_fmt", "yuv420p", "-r", &fps_s, "-g", &gop]);
    // Cores no padrão de vídeo HD, para não ficarem lavadas no celular
    c.args([
        "-colorspace",
        "bt709",
        "-color_primaries",
        "bt709",
        "-color_trc",
        "bt709",
        "-color_range",
        "tv",
    ]);
    c.args([
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ar",
        "48000",
        "-movflags",
        "+faststart",
        "-shortest",
    ]);
    c.args(["-threads", &cfg.threads().to_string()]);
    c.arg(&out);
    proc::run(c, cancel, |line| {
        if let Some(v) = line
            .strip_prefix("out_time_us=")
            .or_else(|| line.strip_prefix("out_time_ms="))
            && let Ok(us) = v.trim().parse::<f64>()
        {
            let f = ((us / 1e6) / dur).clamp(0.0, 1.0) as f32;
            progress(if reframed.is_some() {
                REFRAME_SHARE + (1.0 - REFRAME_SHARE) * f
            } else {
                f
            });
        }
    })
    .await
    .context("falha ao renderizar o clipe")?;
    if let Some(v) = &reframed {
        let _ = tokio::fs::remove_file(v).await;
    }

    // Miniatura
    let mut c = crate::proc::command(paths::ffmpeg());
    c.current_dir(job.work);
    c.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-ss",
        &format!("{:.2}", (dur * 0.3).min(3.0)),
        "-i",
        &out,
    ]);
    c.args([
        "-frames:v",
        "1",
        "-vf",
        "scale=360:-2",
        "-q:v",
        "4",
        &format!("{}.jpg", job.name),
    ]);
    let _ = proc::output(c).await;
    Ok(framing_desc)
}

//! Renderização dos clipes com ffmpeg: corte, reenquadramento e legendas animadas.

use crate::config::Config;
use crate::face;
use crate::paths;
use crate::proc::{self, ProbeInfo};
use crate::transcribe::{Transcript, Word};
use anyhow::{Context, Result};
use std::fmt::Write as _;
use std::path::Path;
use tokio_util::sync::CancellationToken;

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
pub fn build_ass(cfg: &Config, words: &[Word], start: f64, end: f64, w: u32, h: u32) -> String {
    let scale = w.min(h) as f64 / 1080.0;
    let size = (cfg.sub_size as f64 * scale).round();
    let vertical = h > w;
    let (align, margin_v) = match cfg.sub_position.as_str() {
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
            let _ = writeln!(
                s,
                "Dialogue: 0,{},{},Default,,0,0,0,,{}",
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
}

pub async fn render(
    job: RenderJob<'_>,
    cancel: &CancellationToken,
    mut progress: impl FnMut(f32),
) -> Result<()> {
    let cfg = job.cfg;
    let (w, h) = cfg.out_size();
    let dur = (job.end - job.start).max(0.5);
    let mut graph;
    let mut inputs: Vec<String> = vec![];

    if !job.info.has_video {
        // Só áudio (podcast): fundo escuro + legendas
        inputs.extend([
            "-f".into(),
            "lavfi".into(),
            "-t".into(),
            format!("{dur:.3}"),
            "-i".into(),
            format!("color=c=0x111318:s={w}x{h}:r=30"),
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
        graph = match mode {
            "blur" => format!(
                "[0:v]split=2[a][b];[a]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},boxblur=20:2[bg];\
[b]scale={w}:{h}:force_original_aspect_ratio=decrease[fg];[bg][fg]overlay=(W-w)/2:(H-h)/2,setsar=1[v0]"
            ),
            "face" => {
                let dets = face::track(job.src, job.start, dur, job.info.width, job.info.height)
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("rastreamento de rosto falhou: {e:#}");
                        vec![]
                    });
                let path = face::camera_path(&dets, w as f64 / sw as f64);
                let max_x = (sw - w) as f64;
                let to_x = |c: f64| (c * sw as f64 - w as f64 / 2.0).clamp(0.0, max_x).round();
                let x0 = path
                    .first()
                    .map(|c| to_x(*c))
                    .unwrap_or((max_x / 2.0).round());
                let mut cmds = String::new();
                if path.len() > 1 {
                    // Interpola a cada 0,1 s para um movimento suave
                    let total = (dur / 0.1).ceil() as usize;
                    let mut prev = -1.0;
                    for k in 0..=total {
                        let t = k as f64 * 0.1;
                        let fi = t / face::STEP;
                        let i = (fi.floor() as usize).min(path.len() - 1);
                        let j = (i + 1).min(path.len() - 1);
                        let f = fi - fi.floor();
                        let x = to_x(path[i] * (1.0 - f) + path[j] * f);
                        if x != prev {
                            let _ = writeln!(cmds, "{t:.2} crop@cf x {x};");
                            prev = x;
                        }
                    }
                }
                let cmd_name = format!("{}.cmd", job.name);
                tokio::fs::write(job.work.join(&cmd_name), &cmds).await?;
                if cmds.is_empty() {
                    format!("[0:v]scale={sw}:{h},crop@cf={w}:{h}:{x0}:0,setsar=1[v0]")
                } else {
                    format!(
                        "[0:v]scale={sw}:{h},sendcmd=f={cmd_name},crop@cf={w}:{h}:{x0}:0,setsar=1[v0]"
                    )
                }
            }
            _ => format!(
                "[0:v]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1[v0]"
            ),
        };
    }

    if cfg.subtitles && !job.transcript.words.is_empty() {
        let ass_name = format!("{}.ass", job.name);
        let ass = build_ass(cfg, &job.transcript.words, job.start, job.end, w, h);
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
    c.args([
        "-ss",
        &format!("{:.3}", job.start),
        "-t",
        &format!("{dur:.3}"),
        "-i",
    ])
    .arg(job.src);
    c.args(&inputs);
    c.args(["-filter_complex", &graph, "-map", "[v]", "-map", "0:a?"]);
    c.args([
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "21", "-pix_fmt", "yuv420p", "-r", "30",
    ]);
    c.args([
        "-c:a",
        "aac",
        "-b:a",
        "160k",
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
            progress(((us / 1e6) / dur).clamp(0.0, 1.0) as f32);
        }
    })
    .await
    .context("falha ao renderizar o clipe")?;

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
    Ok(())
}

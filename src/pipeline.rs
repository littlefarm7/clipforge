//! Fluxo completo: baixar → transcrever → escolher trechos → renderizar.

use crate::highlights;
use crate::paths;
use crate::proc;
use crate::render::{self, RenderJob};
use crate::state::{App, Clip, Job};
use crate::transcribe::{self, Transcript};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Processa a fila, um trabalho por vez (o processamento já usa todos os núcleos).
pub async fn worker(app: App, mut rx: mpsc::UnboundedReceiver<Job>) {
    while let Some(job) = rx.recv().await {
        let id = match &job {
            Job::Full(id) | Job::Clip(id, _) => id.clone(),
        };
        let cancel = app.cancel_token(&id);
        if cancel.is_cancelled() || app.get(&id).is_none() {
            continue;
        }
        let res = match &job {
            Job::Full(id) => run_full(&app, id, &cancel).await,
            Job::Clip(id, idx) => run_clip(&app, id, *idx, &cancel).await,
        };
        let cancelled = cancel.is_cancelled();
        match res {
            Ok(()) => app.update(&id, true, |p| {
                p.status = "done".into();
                p.stage = "Concluído".into();
                p.progress = 1.0;
                p.error = None;
            }),
            Err(e) => {
                eprintln!("[{id}] erro: {e:#}");
                app.update(&id, true, |p| {
                    p.status = if cancelled {
                        "cancelled".into()
                    } else {
                        "error".into()
                    };
                    p.error = Some(if cancelled {
                        "Cancelado pelo usuário.".into()
                    } else {
                        format!("{e:#}")
                    });
                    for c in p.clips.iter_mut() {
                        if c.status == "rendering" || c.status == "pending" {
                            c.status = "error".into();
                        }
                    }
                });
            }
        }
        app.cancels.lock().unwrap().remove(&id);
    }
}

fn stage(app: &App, id: &str, stage: &str, progress: f32) {
    app.update(id, false, |p| {
        p.stage = stage.to_string();
        p.progress = progress;
    });
}

pub fn find_source(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Ignora metadados e arquivos intermediários do yt-dlp (source.f137.mp4, .part...)
            let parts: Vec<&str> = n.split('.').collect();
            let intermediate = parts.len() > 2
                && parts[1].starts_with('f')
                && parts[1][1..].chars().all(|c| c.is_ascii_digit());
            n.starts_with("source.")
                && !intermediate
                && !n.ends_with(".json")
                && !n.ends_with(".part")
                && !n.ends_with(".ytdl")
                && !n.ends_with(".upload")
        })
}

/// Divide argumentos respeitando aspas simples e duplas.
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for ch in s.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(ch);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

async fn download(
    app: &App,
    id: &str,
    url: &str,
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    stage(app, id, "Baixando vídeo", 0.0);
    let cfg = app.config();
    let height = if cfg.quality == "720" { 720 } else { 1080 };
    let mut c = crate::proc::command(paths::ytdlp());
    c.current_dir(dir);
    c.args([
        "--newline",
        "--no-playlist",
        "--no-mtime",
        "--write-info-json",
        "--restrict-filenames",
    ]);
    c.args([
        "-f",
        &format!("bv*[height<={height}]+ba/b[height<={height}]/bv*+ba/b"),
    ]);
    c.args(["--merge-output-format", "mp4", "-o", "source.%(ext)s"]);
    c.arg("--ffmpeg-location").arg(paths::ffmpeg());
    c.args(split_args(&cfg.ytdlp_args));
    // O deno embutido resolve os desafios JavaScript do YouTube; precisa estar no PATH
    if let Some(dir) = paths::bundled_bin_dir() {
        let mut dirs = vec![dir];
        dirs.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        if let Ok(p) = std::env::join_paths(dirs) {
            c.env("PATH", p);
        }
    }
    c.arg("--").arg(url);
    let mut part = 0;
    let mut last = 0.0f32;
    proc::run(c, cancel, |line| {
        if let Some(rest) = line.strip_prefix("[download]") {
            if let Some(pct) = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.strip_suffix('%'))
                .and_then(|s| s.parse::<f32>().ok())
            {
                let pct = pct / 100.0;
                if pct + 0.2 < last {
                    part += 1; // segunda parte (áudio)
                }
                last = pct;
                let label = if part == 0 {
                    "Baixando vídeo"
                } else {
                    "Baixando áudio"
                };
                stage(app, id, label, pct);
            }
        } else if line.starts_with("[Merger]") {
            stage(app, id, "Juntando áudio e vídeo", 1.0);
        }
    })
    .await
    .context(
        "não foi possível baixar o vídeo. Tente atualizar o yt-dlp em Configurações → Sistema",
    )?;
    if let Ok(s) = std::fs::read_to_string(dir.join("source.info.json"))
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
        && let Some(t) = v["title"].as_str()
    {
        let t = t.to_string();
        app.update(id, true, |p| p.title = t);
    }
    find_source(dir).context("arquivo baixado não encontrado")
}

async fn run_full(app: &App, id: &str, cancel: &CancellationToken) -> Result<()> {
    let cfg = app.config();
    let dir = App::project_dir(id);
    tokio::fs::create_dir_all(&dir).await?;
    app.update(id, true, |p| {
        p.status = "running".into();
        p.error = None;
    });
    let project = app.get(id).context("projeto não existe")?;

    // 1. Vídeo de origem
    let src = match find_source(&dir) {
        Some(s) => s,
        None => {
            if project.source.starts_with("http://") || project.source.starts_with("https://") {
                download(app, id, &project.source, &dir, cancel).await?
            } else {
                bail!("arquivo de vídeo de origem não encontrado");
            }
        }
    };
    let info = proc::probe(&src)
        .await
        .context("não foi possível ler o vídeo")?;
    if info.duration <= 1.0 {
        bail!("o vídeo é muito curto ou não pôde ser lido");
    }
    app.update(id, true, |p| p.duration = info.duration);

    // 2. Transcrição (reaproveitada se já existir)
    let tpath = dir.join("transcript.json");
    let transcript: Transcript = match tokio::fs::read(&tpath)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        Some(t) => t,
        None => {
            stage(app, id, "Transcrevendo", 0.0);
            let t = transcribe::transcribe(&cfg, &src, &dir, cancel, |pr, label| {
                stage(app, id, label, pr)
            })
            .await?;
            if t.words.is_empty() {
                bail!("nenhuma fala detectada no vídeo");
            }
            tokio::fs::write(&tpath, serde_json::to_vec(&t)?).await?;
            t
        }
    };
    let lang = transcript.language.clone();
    app.update(id, true, |p| p.language = lang);

    // 3. Melhores trechos
    stage(app, id, "Analisando", 0.0);
    let cands = highlights::find(&cfg, &transcript, info.duration, cancel, |pr, label| {
        stage(app, id, label, pr)
    })
    .await?;
    if cands.is_empty() {
        bail!("nenhum trecho adequado foi encontrado (tente diminuir a duração mínima)");
    }
    // Remove clipes antigos
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("clip_") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let clips: Vec<Clip> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| Clip {
            index: i as u32,
            name: format!("clip_{:02}", i + 1),
            start: c.start,
            end: c.end,
            title: c.title.clone(),
            score: c.score,
            reason: c.reason.clone(),
            text: transcript.text_between(c.start, c.end),
            status: "pending".into(),
            ..Default::default()
        })
        .collect();
    let n = clips.len();
    app.update(id, true, |p| p.clips = clips);

    // 4. Renderização
    for i in 0..n {
        if cancel.is_cancelled() {
            bail!("Cancelado");
        }
        render_one(
            app,
            id,
            i as u32,
            &src,
            &info,
            &transcript,
            &dir,
            cancel,
            Some((i, n)),
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn render_one(
    app: &App,
    id: &str,
    idx: u32,
    src: &Path,
    info: &proc::ProbeInfo,
    transcript: &Transcript,
    dir: &Path,
    cancel: &CancellationToken,
    batch: Option<(usize, usize)>,
) -> Result<()> {
    let cfg = app.config();
    let clip = app
        .get(id)
        .and_then(|p| p.clips.into_iter().find(|c| c.index == idx))
        .context("clipe não encontrado")?;
    let label = match batch {
        Some((i, n)) => format!("Gerando clipe {}/{}", i + 1, n),
        None => format!("Renderizando clipe {}", idx + 1),
    };
    let set_clip = |f: &dyn Fn(&mut Clip)| {
        app.update(id, false, |p| {
            if let Some(c) = p.clips.iter_mut().find(|c| c.index == idx) {
                f(c);
            }
        })
    };
    set_clip(&|c| {
        c.status = "rendering".into();
        c.progress = 0.0;
        c.error = None;
    });
    stage(
        app,
        id,
        &label,
        batch.map(|(i, n)| i as f32 / n as f32).unwrap_or(0.0),
    );
    let job = RenderJob {
        cfg: &cfg,
        src,
        info,
        transcript,
        work: dir,
        name: &clip.name,
        title: &clip.title,
        start: clip.start,
        end: clip.end,
    };
    let res = render::render(job, cancel, |pr| {
        set_clip(&|c| c.progress = pr);
        let total = match batch {
            Some((i, n)) => (i as f32 + pr) / n as f32,
            None => pr,
        };
        stage(app, id, &label, total);
    })
    .await;
    match res {
        Ok(()) => {
            app.update(id, true, |p| {
                if let Some(c) = p.clips.iter_mut().find(|c| c.index == idx) {
                    c.status = "done".into();
                    c.progress = 1.0;
                    c.rev += 1;
                }
            });
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e:#}");
            app.update(id, true, |p| {
                if let Some(c) = p.clips.iter_mut().find(|c| c.index == idx) {
                    c.status = "error".into();
                    c.error = Some(msg);
                }
            });
            if cancel.is_cancelled() {
                return Err(e);
            }
            // Um clipe com erro não interrompe os demais em lote
            if batch.is_some() { Ok(()) } else { Err(e) }
        }
    }
}

async fn run_clip(app: &App, id: &str, idx: u32, cancel: &CancellationToken) -> Result<()> {
    let dir = App::project_dir(id);
    app.update(id, true, |p| {
        p.status = "running".into();
        p.error = None;
    });
    let src = find_source(&dir).context("vídeo de origem não encontrado")?;
    let info = proc::probe(&src).await?;
    let transcript: Transcript = tokio::fs::read(dir.join("transcript.json"))
        .await
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let text = transcript.text_between(
        app.get(id)
            .and_then(|p| p.clips.iter().find(|c| c.index == idx).map(|c| c.start))
            .unwrap_or(0.0),
        app.get(id)
            .and_then(|p| p.clips.iter().find(|c| c.index == idx).map(|c| c.end))
            .unwrap_or(0.0),
    );
    app.update(id, false, |p| {
        if let Some(c) = p.clips.iter_mut().find(|c| c.index == idx) {
            c.text = text;
        }
    });
    render_one(app, id, idx, &src, &info, &transcript, &dir, cancel, None).await
}

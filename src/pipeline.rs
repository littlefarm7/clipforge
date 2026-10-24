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
                    let raw = format!("{e:#}");
                    p.error = Some(if cancelled {
                        "Você cancelou este projeto.".into()
                    } else {
                        crate::friendly::error(&raw)
                    });
                    p.error_detail = (!cancelled).then_some(raw);
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

/// Atualiza o yt-dlp (os sites mudam com frequência). Retorna a última linha da saída.
pub async fn update_ytdlp() -> Result<String> {
    let bin = paths::user_bin_dir().join(paths::exe("yt-dlp"));
    if !bin.is_file() {
        bail!("o yt-dlp não está na pasta do usuário");
    }
    let mut c = crate::proc::command(&bin);
    c.arg("-U");
    let out = tokio::time::timeout(std::time::Duration::from_secs(180), proc::output(c))
        .await
        .context("a atualização do yt-dlp demorou demais")??;
    let _ = std::fs::write(
        paths::data_dir().join("ytdlp-updated"),
        crate::state::now().to_string(),
    );
    Ok(String::from_utf8_lossy(&out)
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .to_string())
}

/// Atualiza o yt-dlp em segundo plano se a última atualização tiver mais de 2 dias.
pub async fn auto_update_ytdlp() {
    let last: u64 = std::fs::read_to_string(paths::data_dir().join("ytdlp-updated"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if crate::state::now().saturating_sub(last) > 2 * 86400 {
        match update_ytdlp().await {
            Ok(msg) => eprintln!("yt-dlp: {msg}"),
            Err(e) => eprintln!("não foi possível atualizar o yt-dlp: {e:#}"),
        }
    }
}

/// O site pediu login: vale tentar com o login salvo no navegador.
fn needs_login(msg: &str) -> bool {
    let t = msg.to_lowercase();
    [
        "sign in to confirm",
        "login required",
        "use --cookies",
        "confirm your age",
        "age-restricted",
        "members-only",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// Erro típico de yt-dlp desatualizado (o site mudou).
fn looks_outdated(msg: &str) -> bool {
    let t = msg.to_lowercase();
    [
        "unable to extract",
        "nsig",
        "signature",
        "http error 403",
        "requested format is not available",
        "please report this issue",
        "unable to download webpage",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// Baixa o vídeo e resolve sozinho os problemas mais comuns:
/// atualiza o yt-dlp quando o site mudou e usa o login do navegador quando o site pede.
async fn download(
    app: &App,
    id: &str,
    url: &str,
    dir: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf> {
    let first = match download_once(app, id, url, dir, cancel, &[]).await {
        Ok(p) => return Ok(p),
        Err(e) => e,
    };
    if cancel.is_cancelled() {
        return Err(first);
    }
    let mut msg = format!("{first:#}");
    if looks_outdated(&msg) && !needs_login(&msg) {
        stage(app, id, "Atualizando o baixador de vídeos", 0.0);
        if update_ytdlp().await.is_ok() {
            match download_once(app, id, url, dir, cancel, &[]).await {
                Ok(p) => return Ok(p),
                Err(e) => msg = format!("{e:#}"),
            }
        }
    }
    if needs_login(&msg) {
        for browser in [
            "firefox", "chrome", "edge", "brave", "chromium", "opera", "vivaldi",
        ] {
            if cancel.is_cancelled() {
                break;
            }
            stage(app, id, "Usando o login do seu navegador", 0.0);
            if let Ok(p) = download_once(
                app,
                id,
                url,
                dir,
                cancel,
                &["--cookies-from-browser", browser],
            )
            .await
            {
                return Ok(p);
            }
        }
    }
    Err(first)
}

async fn download_once(
    app: &App,
    id: &str,
    url: &str,
    dir: &Path,
    cancel: &CancellationToken,
    extra: &[&str],
) -> Result<PathBuf> {
    stage(app, id, "Baixando vídeo", 0.0);
    let cfg = app.config();
    // Para clipes verticais/quadrados o vídeo é recortado: baixar em resolução maior
    // (até 4K) deixa o recorte nítido em vez de ampliado.
    let height = match (cfg.quality.as_str(), cfg.aspect.as_str()) {
        ("720", _) => 1080,
        (_, "16:9") => 1080,
        _ => 2160,
    };
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
        // Prefere imagem SDR (HDR fica com cores lavadas depois de convertido)
        &format!(
            "bv*[height<={height}][dynamic_range=SDR]+ba/bv*[height<={height}]+ba/b[height<={height}]/bv*+ba/b"
        ),
    ]);
    c.args(["--merge-output-format", "mp4", "-o", "source.%(ext)s"]);
    c.arg("--ffmpeg-location").arg(paths::ffmpeg());
    c.args(split_args(&cfg.ytdlp_args));
    c.args(extra);
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
    .context("não foi possível baixar o vídeo (yt-dlp)")?;
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
        p.error_detail = None;
        p.notice = None;
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
            if cfg.stt_provider != "api" {
                let label = "Preparando a transcrição (só na primeira vez)";
                stage(app, id, label, 0.0);
                crate::models::ensure(app, "whisper", &cfg.whisper_model, cancel, |pr| {
                    stage(app, id, label, pr)
                })
                .await?;
            }
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
    let mut cfg = cfg;
    let mut ai_notice = None;
    if cfg.llm_provider == "local" {
        let label = "Preparando a inteligência artificial (só na primeira vez)";
        stage(app, id, label, 0.0);
        if let Err(e) = crate::models::ensure(app, "llm", &cfg.llm_local_model, cancel, |pr| {
            stage(app, id, label, pr)
        })
        .await
        {
            if cancel.is_cancelled() {
                return Err(e);
            }
            // Sem a IA, os cortes são escolhidos automaticamente
            ai_notice = Some(crate::friendly::ai_notice(&format!("baixar: {e:#}")));
            cfg.llm_provider = "none".into();
        }
    }
    stage(app, id, "Analisando", 0.0);
    let (cands, notice) =
        highlights::find(&cfg, &transcript, info.duration, cancel, |pr, label| {
            stage(app, id, label, pr)
        })
        .await?;
    let ai_notice = ai_notice.or(notice);
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
    let wanted = cfg.max_clips.max(1) as usize;
    let count_notice = (n < wanted).then(|| {
        format!(
            "Foram criados {n} de {wanted} clipes: o vídeo não tem trechos suficientes de {:.0}–{:.0} segundos \
sem repetir conteúdo. Para ter mais clipes, escolha uma duração mais curta.",
            cfg.min_len, cfg.max_len
        )
    });
    let notice = match (ai_notice, count_notice) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    };
    app.update(id, true, |p| {
        p.clips = clips;
        p.notice = notice;
    });

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

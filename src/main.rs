//! ClipForge — gera clipes curtos e virais a partir de vídeos longos, 100% local.

// No Windows o app roda sem janela de console (a interface é a página no navegador).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod config;
mod face;
mod highlights;
mod llm;
mod models;
mod paths;
mod pipeline;
mod proc;
mod render;
mod state;
mod transcribe;

use axum::extract::{DefaultBodyLimit, Multipart, Path as AxPath, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use config::Config;
use serde::Deserialize;
use serde_json::{Value, json};
use state::{App, Job, Project, now};
use tokio::io::AsyncWriteExt;

pub static FONTS: &[(&str, &[u8])] = &[
    (
        "Anton-Regular.ttf",
        include_bytes!("../assets/fonts/Anton-Regular.ttf"),
    ),
    (
        "Poppins-Bold.ttf",
        include_bytes!("../assets/fonts/Poppins-Bold.ttf"),
    ),
];

static INDEX: &str = include_str!("../assets/index.html");

type ApiResult = Result<Json<Value>, ApiError>;

struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("{:#}", self.0)})),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        ApiError(e.into())
    }
}

fn err(msg: &str) -> ApiError {
    ApiError(anyhow::anyhow!(msg.to_string()))
}

fn new_id() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{}-{}", now(), &u[..8])
}

// ---------- Projetos ----------

async fn api_state(State(app): State<App>) -> Json<Value> {
    let mut list: Vec<Project> = app.projects.lock().unwrap().values().cloned().collect();
    list.sort_by(|a, b| b.created.cmp(&a.created).then(b.id.cmp(&a.id)));
    let downloads = app.downloads.lock().unwrap().clone();
    Json(json!({"projects": list, "downloads": downloads}))
}

#[derive(Deserialize)]
struct NewProject {
    url: String,
}

async fn api_new_project(State(app): State<App>, Json(req): Json<NewProject>) -> ApiResult {
    let url = req.url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(err("Cole um link válido (começando com https://)."));
    }
    let id = new_id();
    app.insert(Project {
        id: id.clone(),
        title: url.clone(),
        source: url,
        created: now(),
        status: "queued".into(),
        stage: "Na fila".into(),
        ..Default::default()
    });
    app.enqueue(Job::Full(id.clone()));
    Ok(Json(json!({"id": id})))
}

async fn api_upload(State(app): State<App>, mut mp: Multipart) -> ApiResult {
    while let Some(mut field) = mp.next_field().await? {
        if field.name() != Some("file") {
            continue;
        }
        let fname = field.file_name().unwrap_or("video.mp4").to_string();
        let ext = std::path::Path::new(&fname)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()) && e.len() <= 5)
            .unwrap_or_else(|| "mp4".into());
        let id = new_id();
        let dir = App::project_dir(&id);
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join("source.upload");
        let mut f = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = field.chunk().await? {
            f.write_all(&chunk).await?;
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, dir.join(format!("source.{ext}"))).await?;
        let title = std::path::Path::new(&fname)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or(fname.clone());
        app.insert(Project {
            id: id.clone(),
            title,
            source: fname,
            created: now(),
            status: "queued".into(),
            stage: "Na fila".into(),
            ..Default::default()
        });
        app.enqueue(Job::Full(id.clone()));
        return Ok(Json(json!({"id": id})));
    }
    Err(err("nenhum arquivo enviado"))
}

fn valid_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(err("id inválido"));
    }
    Ok(())
}

async fn api_cancel(State(app): State<App>, AxPath(id): AxPath<String>) -> ApiResult {
    valid_id(&id)?;
    app.cancel(&id);
    app.update(&id, true, |p| {
        if p.status == "queued" || p.status == "running" {
            p.status = "cancelled".into();
            p.error = Some("Cancelado pelo usuário.".into());
        }
    });
    Ok(Json(json!({"ok": true})))
}

async fn api_retry(State(app): State<App>, AxPath(id): AxPath<String>) -> ApiResult {
    valid_id(&id)?;
    let p = app.get(&id).ok_or_else(|| err("projeto não encontrado"))?;
    if p.status == "running" || p.status == "queued" {
        return Err(err("o projeto já está em processamento"));
    }
    app.update(&id, true, |p| {
        p.status = "queued".into();
        p.stage = "Na fila".into();
        p.progress = 0.0;
        p.error = None;
    });
    app.enqueue(Job::Full(id));
    Ok(Json(json!({"ok": true})))
}

/// Apaga a transcrição para refazê-la com as configurações atuais.
async fn api_retranscribe(State(app): State<App>, AxPath(id): AxPath<String>) -> ApiResult {
    valid_id(&id)?;
    let _ = tokio::fs::remove_file(App::project_dir(&id).join("transcript.json")).await;
    api_retry(State(app), AxPath(id)).await
}

async fn api_delete(State(app): State<App>, AxPath(id): AxPath<String>) -> ApiResult {
    valid_id(&id)?;
    app.cancel(&id);
    app.projects.lock().unwrap().remove(&id);
    let dir = App::project_dir(&id);
    // Aguarda processos terminarem de liberar os arquivos
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ClipEdit {
    start: f64,
    end: f64,
    title: Option<String>,
}

async fn api_clip_render(
    State(app): State<App>,
    AxPath((id, idx)): AxPath<(String, u32)>,
    Json(req): Json<ClipEdit>,
) -> ApiResult {
    valid_id(&id)?;
    let p = app.get(&id).ok_or_else(|| err("projeto não encontrado"))?;
    if p.status == "running" || p.status == "queued" {
        return Err(err("aguarde o processamento atual terminar"));
    }
    if !(req.end > req.start && req.start >= 0.0) || req.end - req.start > 600.0 {
        return Err(err("tempos inválidos"));
    }
    let end = if p.duration > 0.0 {
        req.end.min(p.duration)
    } else {
        req.end
    };
    app.update(&id, true, |p| {
        if let Some(c) = p.clips.iter_mut().find(|c| c.index == idx) {
            c.start = req.start;
            c.end = end;
            if let Some(t) = &req.title {
                c.title = t.trim().to_string();
            }
            c.status = "pending".into();
        }
        p.status = "queued".into();
        p.stage = "Na fila".into();
    });
    app.enqueue(Job::Clip(id, idx));
    Ok(Json(json!({"ok": true})))
}

// ---------- Configuração ----------

async fn api_get_config(State(app): State<App>) -> Json<Config> {
    Json(app.config())
}

async fn api_set_config(State(app): State<App>, Json(cfg): Json<Config>) -> ApiResult {
    cfg.save()?;
    *app.config.write().unwrap() = cfg;
    Ok(Json(json!({"ok": true})))
}

async fn api_test_llm(Json(cfg): Json<Config>) -> ApiResult {
    let cancel = tokio_util::sync::CancellationToken::new();
    let llm = llm::Llm::from_config(&cfg, &cancel)
        .await?
        .ok_or_else(|| err("IA desativada (modo sem IA)"))?;
    let r = llm
        .chat(
            "Responda de forma curtíssima.",
            "Diga apenas: OK, estou funcionando.",
            false,
        )
        .await?;
    Ok(Json(json!({"reply": r})))
}

// ---------- Modelos ----------

async fn api_models(State(app): State<App>) -> Json<Value> {
    let mark = |kind: &str, cat: Vec<models::ModelInfo>| {
        let inst = models::installed(kind);
        let cat_files: Vec<&str> = cat.iter().map(|m| m.file).collect();
        let mut v: Vec<Value> = cat
            .iter()
            .map(|m| {
                let mut j = serde_json::to_value(m).unwrap();
                j["installed"] = json!(inst.iter().any(|(f, _)| f == m.file));
                j
            })
            .collect();
        for (f, size) in inst
            .iter()
            .filter(|(f, _)| !cat_files.contains(&f.as_str()))
        {
            v.push(json!({"id": f, "name": f, "file": f, "url": "", "size_mb": size / 1_000_000, "desc": "Modelo personalizado", "installed": true}));
        }
        v
    };
    let downloads = app.downloads.lock().unwrap().clone();
    Json(json!({
        "whisper": mark("whisper", models::whisper_catalog()),
        "llm": mark("llm", models::llm_catalog()),
        "downloads": downloads,
    }))
}

#[derive(Deserialize)]
struct DownloadReq {
    kind: String,
    id: Option<String>,
    url: Option<String>,
}

async fn api_model_download(State(app): State<App>, Json(req): Json<DownloadReq>) -> ApiResult {
    let kind = if req.kind == "whisper" {
        "whisper"
    } else {
        "llm"
    };
    let (url, file) = if let Some(id) = &req.id {
        let cat = if kind == "whisper" {
            models::whisper_catalog()
        } else {
            models::llm_catalog()
        };
        let m = cat
            .into_iter()
            .find(|m| m.id == id)
            .ok_or_else(|| err("modelo desconhecido"))?;
        (m.url.to_string(), m.file.to_string())
    } else {
        let url = req.url.clone().unwrap_or_default().trim().to_string();
        if !url.starts_with("https://") {
            return Err(err("informe um link https:// para o arquivo do modelo"));
        }
        let file = models::safe_file_name(&url)?;
        let ok_ext = if kind == "whisper" {
            file.ends_with(".bin")
        } else {
            file.ends_with(".gguf")
        };
        if !ok_ext {
            return Err(err(if kind == "whisper" {
                "o arquivo deve terminar em .bin"
            } else {
                "o arquivo deve terminar em .gguf"
            }));
        }
        (url, file)
    };
    let key = format!("{kind}/{file}");
    {
        let mut d = app.downloads.lock().unwrap();
        if d.get(&key).is_some_and(|s| !s.done && s.error.is_none()) {
            return Err(err("este download já está em andamento"));
        }
        d.insert(
            key.clone(),
            models::DownloadState {
                kind: kind.into(),
                file: file.clone(),
                ..Default::default()
            },
        );
    }
    let dest = models::dir_for(kind).join(&file);
    let app2 = app.clone();
    tokio::spawn(async move {
        let k = key.clone();
        let res = models::download(&url, &dest, |got, total| {
            if let Some(s) = app2.downloads.lock().unwrap().get_mut(&k) {
                s.downloaded = got;
                s.total = total;
            }
        })
        .await;
        let mut d = app2.downloads.lock().unwrap();
        if let Some(s) = d.get_mut(&key) {
            s.done = true;
            if let Err(e) = res {
                s.error = Some(format!("{e:#}"));
            }
        }
    });
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct DeleteModel {
    kind: String,
    file: String,
}

async fn api_model_delete(State(app): State<App>, Json(req): Json<DeleteModel>) -> ApiResult {
    let file = models::safe_file_name(&req.file)?;
    let kind = if req.kind == "whisper" {
        "whisper"
    } else {
        "llm"
    };
    tokio::fs::remove_file(models::dir_for(kind).join(&file)).await?;
    app.downloads
        .lock()
        .unwrap()
        .remove(&format!("{kind}/{file}"));
    Ok(Json(json!({"ok": true})))
}

// ---------- Sistema ----------

async fn tool_version(cmd: std::path::PathBuf, args: &[&str]) -> String {
    let mut c = proc::command(&cmd);
    c.args(args);
    match tokio::time::timeout(std::time::Duration::from_secs(20), proc::output(c)).await {
        Ok(Ok(out)) => String::from_utf8_lossy(&out)
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect(),
        _ => "não encontrado".into(),
    }
}

async fn api_system() -> Json<Value> {
    let (ff, yt) = tokio::join!(
        tool_version(paths::ffmpeg(), &["-version"]),
        tool_version(paths::ytdlp(), &["--version"])
    );
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "ffmpeg": ff,
        "ytdlp": yt,
        "whisper": paths::whisper_cli().to_string_lossy(),
        "llama": paths::llama_server().to_string_lossy(),
        "avx2": paths::cpu_has_avx2(),
        "threads": Config::default().threads(),
        "data_dir": paths::data_dir().to_string_lossy(),
    }))
}

async fn api_ytdlp_update() -> ApiResult {
    let bin = paths::user_bin_dir().join(paths::exe("yt-dlp"));
    if !bin.is_file() {
        return Err(err(
            "yt-dlp não está na pasta do usuário; não é possível atualizar",
        ));
    }
    let mut c = proc::command(&bin);
    c.arg("-U");
    let out = proc::output(c).await?;
    Ok(Json(json!({"output": String::from_utf8_lossy(&out)})))
}

async fn api_open_folder(AxPath(id): AxPath<String>) -> ApiResult {
    valid_id(&id)?;
    open_external(App::project_dir(&id).as_os_str());
    Ok(Json(json!({"ok": true})))
}

async fn api_shutdown(State(app): State<App>) -> Json<Value> {
    app.shutdown.notify_one();
    Json(json!({"ok": true}))
}

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Html(INDEX))
}

/// Abre um link ou pasta com o programa padrão do sistema.
fn open_external(target: &std::ffi::OsStr) {
    #[cfg(target_os = "windows")]
    let mut c = {
        let mut c = std::process::Command::new("explorer");
        c.arg(target);
        c
    };
    #[cfg(target_os = "macos")]
    let mut c = {
        let mut c = std::process::Command::new("open");
        c.arg(target);
        c
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut c = {
        let mut c = std::process::Command::new("xdg-open");
        // Remove variáveis do AppImage que podem atrapalhar o navegador
        c.arg(target)
            .env_remove("LD_LIBRARY_PATH")
            .env_remove("APPIMAGE")
            .env_remove("APPDIR");
        c
    };
    let _ = c
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn open_browser(url: &str) {
    if std::env::var_os("CLIPFORGE_NO_BROWSER").is_some() {
        return;
    }
    open_external(std::ffi::OsStr::new(url));
}

async fn existing_instance() -> Option<u16> {
    let port: u16 = std::fs::read_to_string(paths::data_dir().join("port"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let c = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let r = c
        .get(format!("http://127.0.0.1:{port}/api/ping"))
        .send()
        .await
        .ok()?;
    (r.text().await.ok()? == "clipforge").then_some(port)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    paths::init();
    if let Some(port) = existing_instance().await {
        let url = format!("http://127.0.0.1:{port}/");
        println!("ClipForge já está aberto em {url}");
        open_browser(&url);
        return Ok(());
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let app = App::new(tx);
    tokio::spawn(pipeline::worker(app.clone(), rx));

    let files = tower_http::services::ServeDir::new(paths::projects_dir());
    let router = Router::new()
        .route("/", get(index))
        .route("/api/ping", get(|| async { "clipforge" }))
        .route("/api/state", get(api_state))
        .route("/api/projects", post(api_new_project))
        .route("/api/upload", post(api_upload))
        .route("/api/projects/{id}/cancel", post(api_cancel))
        .route("/api/projects/{id}/retry", post(api_retry))
        .route("/api/projects/{id}/retranscribe", post(api_retranscribe))
        .route("/api/projects/{id}/delete", post(api_delete))
        .route("/api/projects/{id}/open", post(api_open_folder))
        .route(
            "/api/projects/{id}/clips/{idx}/render",
            post(api_clip_render),
        )
        .route("/api/config", get(api_get_config).post(api_set_config))
        .route("/api/test-llm", post(api_test_llm))
        .route("/api/models", get(api_models))
        .route("/api/models/download", post(api_model_download))
        .route("/api/models/delete", post(api_model_delete))
        .route("/api/system", get(api_system))
        .route("/api/ytdlp-update", post(api_ytdlp_update))
        .route("/api/shutdown", post(api_shutdown))
        .nest_service("/files", files)
        .layer(DefaultBodyLimit::disable())
        .with_state(app.clone());

    let port_env: Option<u16> = std::env::var("CLIPFORGE_PORT")
        .ok()
        .and_then(|p| p.parse().ok());
    let mut listener = None;
    for port in port_env.into_iter().chain(7861..7900) {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            listener = Some(l);
            break;
        }
    }
    let listener =
        listener.ok_or_else(|| anyhow::anyhow!("nenhuma porta livre entre 7861 e 7899"))?;
    let port = listener.local_addr()?.port();
    let _ = std::fs::write(paths::data_dir().join("port"), port.to_string());
    let url = format!("http://127.0.0.1:{port}/");
    println!("ClipForge rodando em {url}  (feche pelo botão \"Sair\" na página ou com Ctrl+C)");
    open_browser(&url);

    let app2 = app.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = app2.shutdown.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        })
        .await?;
    let _ = std::fs::remove_file(paths::data_dir().join("port"));
    // Cancela trabalhos em andamento (encerra ffmpeg/whisper/llama)
    for (_, t) in app.cancels.lock().unwrap().drain() {
        t.cancel();
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    Ok(())
}

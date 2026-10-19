//! Estado global: projetos, fila de trabalhos, configuração e downloads de modelos.

use crate::config::Config;
use crate::models::DownloadState;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Clip {
    pub index: u32,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub score: u32,
    pub reason: String,
    pub text: String,
    /// "pending", "rendering", "done", "error"
    pub status: String,
    pub progress: f32,
    pub error: Option<String>,
    /// Incrementa a cada renderização (evita cache do navegador)
    pub rev: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Project {
    pub id: String,
    pub title: String,
    pub source: String,
    pub created: u64,
    /// "queued", "running", "done", "error", "cancelled"
    pub status: String,
    pub stage: String,
    pub progress: f32,
    pub error: Option<String>,
    pub duration: f64,
    pub language: String,
    pub clips: Vec<Clip>,
}

#[derive(Debug, Clone)]
pub enum Job {
    /// Processa o projeto inteiro (reaproveita download/transcrição existentes).
    Full(String),
    /// Re-renderiza um clipe.
    Clip(String, u32),
}

pub struct Inner {
    pub projects: Mutex<HashMap<String, Project>>,
    pub config: RwLock<Config>,
    pub queue: mpsc::UnboundedSender<Job>,
    pub cancels: Mutex<HashMap<String, CancellationToken>>,
    pub downloads: Mutex<HashMap<String, DownloadState>>,
    pub shutdown: tokio::sync::Notify,
}

#[derive(Clone)]
pub struct App(pub Arc<Inner>);

impl std::ops::Deref for App {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl App {
    pub fn new(queue: mpsc::UnboundedSender<Job>) -> Self {
        let mut projects = HashMap::new();
        for e in std::fs::read_dir(paths::projects_dir()).into_iter().flatten().flatten() {
            let p = e.path().join("project.json");
            if let Some(mut pr) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Project>(&s).ok()) {
                // Trabalhos interrompidos ao fechar o app
                if pr.status == "running" || pr.status == "queued" {
                    pr.status = "error".into();
                    pr.error = Some("Interrompido (o aplicativo foi fechado). Clique em \"Tentar de novo\".".into());
                }
                for c in pr.clips.iter_mut() {
                    if c.status == "rendering" || c.status == "pending" {
                        c.status = if std::path::Path::new(&e.path().join(format!("{}.mp4", c.name))).is_file() { "done".into() } else { "error".into() };
                    }
                }
                projects.insert(pr.id.clone(), pr);
            }
        }
        App(Arc::new(Inner {
            projects: Mutex::new(projects),
            config: RwLock::new(Config::load()),
            queue,
            cancels: Mutex::new(HashMap::new()),
            downloads: Mutex::new(HashMap::new()),
            shutdown: tokio::sync::Notify::new(),
        }))
    }

    pub fn config(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn project_dir(id: &str) -> PathBuf {
        paths::projects_dir().join(id)
    }

    pub fn get(&self, id: &str) -> Option<Project> {
        self.projects.lock().unwrap().get(id).cloned()
    }

    /// Altera um projeto; com `persist` grava o project.json.
    pub fn update(&self, id: &str, persist: bool, f: impl FnOnce(&mut Project)) {
        let snapshot = {
            let mut map = self.projects.lock().unwrap();
            let Some(p) = map.get_mut(id) else { return };
            f(p);
            persist.then(|| p.clone())
        };
        if let Some(p) = snapshot {
            Self::save(&p);
        }
    }

    pub fn save(p: &Project) {
        let dir = Self::project_dir(&p.id);
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(s) = serde_json::to_string_pretty(p) {
            let tmp = dir.join("project.json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("project.json"));
            }
        }
    }

    pub fn insert(&self, p: Project) {
        Self::save(&p);
        self.projects.lock().unwrap().insert(p.id.clone(), p);
    }

    pub fn cancel_token(&self, id: &str) -> CancellationToken {
        self.cancels.lock().unwrap().entry(id.to_string()).or_default().clone()
    }

    pub fn cancel(&self, id: &str) {
        if let Some(t) = self.cancels.lock().unwrap().remove(id) {
            t.cancel();
        }
    }

    pub fn enqueue(&self, job: Job) {
        let _ = self.queue.send(job);
    }
}

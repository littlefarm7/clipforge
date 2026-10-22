//! Diretórios de dados e localização das ferramentas externas (ffmpeg, yt-dlp, whisper, llama).

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("clipforge")
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("clipforge")
        .join("config.json")
}

pub fn projects_dir() -> PathBuf {
    data_dir().join("projects")
}

pub fn whisper_models_dir() -> PathBuf {
    data_dir().join("models").join("whisper")
}

pub fn llm_models_dir() -> PathBuf {
    data_dir().join("models").join("llm")
}

/// Pasta gravável com cópias de ferramentas que o usuário pode atualizar (yt-dlp).
pub fn user_bin_dir() -> PathBuf {
    data_dir().join("bin")
}

pub fn fonts_dir() -> PathBuf {
    data_dir().join("fonts")
}

/// Pasta com os binários embutidos (AppImage: definida pelo AppRun; Windows/macOS: pasta do executável).
pub fn bundled_bin_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CLIPFORGE_BIN_DIR") {
        return Some(PathBuf::from(d));
    }
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

pub fn cpu_has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("fma")
            && is_x86_feature_detected!("f16c")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Nome do executável com a extensão do sistema (".exe" no Windows).
pub fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn find_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let p = dir.join(exe(name));
    p.is_file().then_some(p)
}

/// Pastas com ferramentas embutidas. O llama.cpp do Windows e do macOS fica numa
/// subpasta própria porque traz bibliotecas (DLL/dylib) que não podem se misturar.
fn bundled_dirs() -> Vec<PathBuf> {
    let Some(d) = bundled_bin_dir() else {
        return vec![];
    };
    vec![d.join("llama"), d.clone(), d.join("tools")]
}

/// Procura uma ferramenta: primeiro na pasta do usuário, depois no pacote do app, por fim no PATH.
pub fn tool(names: &[&str]) -> PathBuf {
    for name in names {
        if let Some(p) = find_in(&user_bin_dir(), name) {
            return p;
        }
        for d in bundled_dirs() {
            if let Some(p) = find_in(&d, name) {
                return p;
            }
        }
    }
    for name in names {
        if let Some(path) = std::env::var_os("PATH") {
            for d in std::env::split_paths(&path) {
                if let Some(p) = find_in(&d, name) {
                    return p;
                }
            }
        }
    }
    PathBuf::from(exe(names[0]))
}

pub fn ffmpeg() -> PathBuf {
    tool(&["ffmpeg"])
}

pub fn ffprobe() -> PathBuf {
    tool(&["ffprobe"])
}

pub fn ytdlp() -> PathBuf {
    tool(&["yt-dlp"])
}

pub fn whisper_cli() -> PathBuf {
    if cpu_has_avx2() {
        tool(&["whisper-cli-avx2", "whisper-cli-generic", "whisper-cli"])
    } else {
        tool(&["whisper-cli-generic", "whisper-cli"])
    }
}

pub fn llama_server() -> PathBuf {
    if cpu_has_avx2() {
        tool(&["llama-server-avx2", "llama-server-generic", "llama-server"])
    } else {
        tool(&["llama-server-generic", "llama-server"])
    }
}

/// Prepara as pastas e copia o yt-dlp e as fontes embutidas para locais graváveis.
pub fn init() {
    for d in [
        projects_dir(),
        whisper_models_dir(),
        llm_models_dir(),
        user_bin_dir(),
        fonts_dir(),
    ] {
        let _ = std::fs::create_dir_all(d);
    }
    // O yt-dlp é copiado para uma pasta gravável para poder se atualizar sozinho
    if let Some(b) = bundled_bin_dir() {
        let src = b.join(exe("yt-dlp"));
        let dst = user_bin_dir().join(exe("yt-dlp"));
        if src.is_file() && !dst.is_file() && std::fs::copy(&src, &dst).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    for (name, bytes) in crate::FONTS {
        let p = fonts_dir().join(name);
        if !p.is_file() {
            let _ = std::fs::write(p, bytes);
        }
    }
}

/// Memória RAM total do computador, em GB (0 se não for possível saber).
pub fn total_ram_gb() -> f64 {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))?
                    .split_whitespace()
                    .nth(1)?
                    .parse::<f64>()
                    .ok()
            })
            .map(|kb| kb / 1024.0 / 1024.0)
            .unwrap_or(0.0)
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
            .map(|b| b / 1024.0 / 1024.0 / 1024.0)
            .unwrap_or(0.0)
    }
    #[cfg(windows)]
    {
        #[repr(C)]
        struct MemoryStatusEx {
            length: u32,
            memory_load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page_file: u64,
            avail_page_file: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended_virtual: u64,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        }
        let mut m = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: a estrutura tem o layout esperado pela API e `length` está preenchido.
        if unsafe { GlobalMemoryStatusEx(&mut m) } != 0 {
            m.total_phys as f64 / 1024.0 / 1024.0 / 1024.0
        } else {
            0.0
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        0.0
    }
}

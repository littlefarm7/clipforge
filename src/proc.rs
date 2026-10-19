//! Execução de processos externos com leitura de progresso linha a linha e cancelamento.

use anyhow::{Context, Result, bail};
use std::collections::VecDeque;
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Cria um comando externo; no Windows, sem abrir uma janela de console.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

async fn pump<R: AsyncRead + Unpin>(mut r: R, tx: mpsc::UnboundedSender<String>) {
    let mut buf = vec![0u8; 16384];
    let mut acc = Vec::new();
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                for &b in &buf[..n] {
                    if b == b'\n' || b == b'\r' {
                        if !acc.is_empty() {
                            let _ = tx.send(String::from_utf8_lossy(&acc).into_owned());
                            acc.clear();
                        }
                    } else {
                        acc.push(b);
                    }
                }
            }
        }
    }
    if !acc.is_empty() {
        let _ = tx.send(String::from_utf8_lossy(&acc).into_owned());
    }
}

/// Executa o comando, chamando `on_line` para cada linha de stdout/stderr.
pub async fn run(
    mut cmd: Command,
    cancel: &CancellationToken,
    mut on_line: impl FnMut(&str),
) -> Result<()> {
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let prog = cmd.as_std().get_program().to_string_lossy().into_owned();
    let short = std::path::Path::new(&prog)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or(prog.clone());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("não foi possível iniciar {prog}"))?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(pump(child.stdout.take().unwrap(), tx.clone()));
    tokio::spawn(pump(child.stderr.take().unwrap(), tx));
    let mut tail: VecDeque<String> = VecDeque::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                bail!("Cancelado");
            }
            line = rx.recv() => match line {
                Some(l) => {
                    on_line(&l);
                    tail.push_back(l);
                    if tail.len() > 12 { tail.pop_front(); }
                }
                None => break,
            }
        }
    }
    let status = child.wait().await?;
    if !status.success() {
        bail!(
            "{short} falhou ({status}):\n{}",
            tail.into_iter().collect::<Vec<_>>().join("\n")
        );
    }
    Ok(())
}

/// Executa e retorna o stdout inteiro (para ffprobe etc.).
pub async fn output(mut cmd: Command) -> Result<Vec<u8>> {
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    let prog = cmd.as_std().get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .await
        .with_context(|| format!("não foi possível iniciar {prog}"))?;
    if !out.status.success() {
        bail!(
            "{prog} falhou: {}",
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(1500)
                .collect::<String>()
        );
    }
    Ok(out.stdout)
}

pub struct ProbeInfo {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_video: bool,
}

pub async fn probe(path: &std::path::Path) -> Result<ProbeInfo> {
    let mut c = command(crate::paths::ffprobe());
    c.args([
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(path);
    let v: serde_json::Value = serde_json::from_slice(&output(c).await?)?;
    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let mut info = ProbeInfo {
        duration,
        width: 0,
        height: 0,
        has_video: false,
    };
    if let Some(streams) = v["streams"].as_array() {
        for s in streams {
            if s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64() != Some(1) {
                let (mut w, mut h) = (
                    s["width"].as_u64().unwrap_or(0) as u32,
                    s["height"].as_u64().unwrap_or(0) as u32,
                );
                // Vídeos de celular podem vir rotacionados
                let rot = s["side_data_list"]
                    .as_array()
                    .and_then(|l| l.iter().find_map(|d| d["rotation"].as_i64()))
                    .or_else(|| s["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
                    .unwrap_or(0);
                if rot.abs() == 90 || rot.abs() == 270 {
                    std::mem::swap(&mut w, &mut h);
                }
                info.width = w;
                info.height = h;
                info.has_video = true;
                break;
            }
        }
    }
    Ok(info)
}

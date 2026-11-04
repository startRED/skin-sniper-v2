//! Skin Sniper v2 — desenho em docs/projeto.md.

// Os módulos de caça ainda são esqueleto: sem chamadores, muito código público fica "morto" por ora.
#![allow(dead_code)]

mod csfloat;
mod db;
mod hunt;
mod notify;
mod pricing;
mod settings;
mod steam;
mod web;

use anyhow::{Context, Result};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const LOG_MAX: u64 = 5 * 1024 * 1024;
const LOG_KEEP: u32 = 3; // sniper.log, sniper.log.1, sniper.log.2

/// Arquivo de log com rotação por tamanho, sem crate: 5 MB × 3.
#[derive(Clone)]
struct RotatingLog(Arc<Mutex<LogInner>>);

struct LogInner {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
}

impl RotatingLog {
    fn open(path: PathBuf) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let size = file.metadata()?.len();
        Ok(RotatingLog(Arc::new(Mutex::new(LogInner {
            path,
            file: Some(file),
            size,
        }))))
    }
}

impl LogInner {
    fn suffixed(&self, n: u32) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(format!(".{n}"));
        s.into()
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None; // o Windows não renomeia arquivo aberto
        for n in (1..LOG_KEEP - 1).rev() {
            let _ = std::fs::rename(self.suffixed(n), self.suffixed(n + 1)); // sobrescreve o mais velho
        }
        let _ = std::fs::rename(&self.path, self.suffixed(1));
        self.file = Some(std::fs::File::create(&self.path)?);
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.size + buf.len() as u64 > LOG_MAX {
            g.rotate()?;
        }
        let n = g
            .file
            .as_mut()
            .ok_or(io::ErrorKind::BrokenPipe)?
            .write(buf)?;
        g.size += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

fn data_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("SNIPER_DATA") {
        return Ok(d.into());
    }
    let exe = std::env::current_exe().context("achar o executável")?;
    Ok(exe.parent().unwrap_or(Path::new(".")).join("data"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let listen: SocketAddr = std::env::var("SNIPER_LISTEN")
        .unwrap_or_else(|_| "0.0.0.0:8788".into())
        .parse()
        .context("SNIPER_LISTEN inválido (esperado ip:porta)")?;
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("criar {}", dir.display()))?;

    let log = RotatingLog::open(dir.join("sniper.log"))?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_writer(move || log.clone())
        .init();

    // Duas instâncias dobrariam o tráfego na Steam: porta ocupada = não sobe.
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| {
            format!("porta {listen} ocupada ou indisponível; já há um Skin Sniper rodando?")
        })
        .inspect_err(|e| tracing::error!("{e:#}"))?;

    let path = dir.join("sniper.db");
    let db = tokio::task::spawn_blocking(move || db::Db::open(&path)).await??;
    db::spawn_retention(db.clone());
    let app = web::AppState::new(db.clone(), settings::SettingsStore::new(db));

    tracing::info!(%listen, data = %dir.display(), "no ar");
    let stop = Arc::new(Notify::new());
    let stop2 = stop.clone();
    let server = axum::serve(listener, web::router(app)).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("Ctrl+C: desligando");
        stop2.notify_one();
    });
    tokio::select! {
        r = server => r?,
        // Conexões SSE abertas seguram o desligamento limpo; depois de 3 s encerra mesmo assim.
        () = async { stop.notified().await; tokio::time::sleep(Duration::from_secs(3)).await } => {
            tracing::warn!("conexões abertas forçadas a fechar");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_rotates_keeping_three_files() {
        let dir = db::tempfile_dir::Dir::new();
        let path = dir.path().join("sniper.log");
        let mut log = RotatingLog::open(path.clone()).unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..16 {
            log.write_all(&chunk).unwrap();
        }
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        assert!(size(&path).unwrap() <= LOG_MAX);
        assert!(size(&dir.path().join("sniper.log.1")).is_some());
        assert!(size(&dir.path().join("sniper.log.2")).is_some());
        assert!(size(&dir.path().join("sniper.log.3")).is_none());
    }
}

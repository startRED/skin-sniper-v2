//! Skin Sniper v2 — desenho em docs/projeto.md.

mod csfloat;
mod db;
mod hunt;
mod notify;
mod pricing;
mod runtime;
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
const LOG_RETRY_BYTES: u64 = 256 * 1024; // após falha de rotação, só tenta de novo depois disso

/// Arquivo de log com rotação por tamanho, sem crate: 5 MB × 3.
#[derive(Clone)]
struct RotatingLog(Arc<Mutex<LogInner>>);

struct LogInner {
    path: PathBuf,
    file: Option<std::fs::File>,
    size: u64,
    retry_at: u64,
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
            retry_at: 0,
        }))))
    }
}

impl LogInner {
    fn suffixed(&self, n: u32) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(format!(".{n}"));
        s.into()
    }

    /// Arquiva o log atual em `.1`. Se algo falhar, o log atual segue intacto e em uso.
    fn rotate(&mut self) -> io::Result<()> {
        self.file = None; // o Windows não renomeia arquivo aberto
        let archived = self.archive();
        if let Err(e) = &archived {
            // Reabre o atual e adia a próxima tentativa, para não renomear a cada linha.
            self.retry_at = self.size + LOG_RETRY_BYTES;
            tracing_fallback(e);
        }
        let reopen = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path);
        self.file = Some(reopen?);
        if archived.is_ok() {
            self.size = 0;
        }
        Ok(())
    }

    fn archive(&self) -> io::Result<()> {
        let (older, newer) = (self.suffixed(2), self.suffixed(1));
        // Só o histórico que não existe é ignorado; qualquer outro erro aborta antes de tocar no atual.
        if newer.exists() {
            match std::fs::remove_file(&older) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            std::fs::rename(&newer, &older)?;
        }
        std::fs::rename(&self.path, &newer)
    }
}

/// O log em si está quebrado; o aviso vai para o stderr.
fn tracing_fallback(e: &io::Error) {
    eprintln!("rotação do log falhou, seguindo no arquivo atual: {e}");
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.size + buf.len() as u64 > LOG_MAX && g.size >= g.retry_at {
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
    let settings = settings::SettingsStore::new(db.clone());
    let mut app = web::AppState::new(db.clone(), settings.clone());
    let steam = Arc::new(steam::client::SteamClient::new()?);
    let csfloat = Arc::new(csfloat::CsfloatClient::new()?);
    let telegram = Arc::new(notify::Telegram::new()?);
    let hunt = hunt::Hunt::new(hunt::HuntDeps {
        db: db.clone(),
        steam: steam.clone(),
        csfloat: csfloat.clone(),
        events: app.events.clone(),
        settings: settings.clone(),
        telegram: Some(telegram.clone()),
    });
    hunt.load().await?;
    let rt = runtime::Runtime::new(
        db,
        settings,
        app.events.clone(),
        hunt.clone(),
        steam,
        csfloat,
        telegram,
    );
    rt.settings_changed().await?;
    rt.spawn_wallet_refresh();
    app.login.spawn_renewal(rt.clone());
    tokio::spawn(hunt::scheduler::run(hunt));
    app.rt = Some(rt);

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

    #[test]
    fn failed_rotation_keeps_current_log() {
        let dir = db::tempfile_dir::Dir::new();
        let path = dir.path().join("sniper.log");
        let mut log = RotatingLog::open(path.clone()).unwrap();
        log.write_all(b"antes\n").unwrap();
        // .1 e .2 viram diretórios não vazios: arquivar .1 em .2 falha.
        for n in [1, 2] {
            let d = dir.path().join(format!("sniper.log.{n}"));
            std::fs::create_dir_all(d.join("x")).unwrap();
        }
        let chunk = vec![b'y'; 1024 * 1024];
        for _ in 0..6 {
            log.write_all(&chunk).unwrap();
        }
        log.write_all(b"depois\n").unwrap();
        log.flush().unwrap();
        let text = std::fs::read(&path).unwrap();
        assert!(text.starts_with(b"antes\n") && text.ends_with(b"depois\n"));
    }
}

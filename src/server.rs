use crate::backup::{self, Progress};
use crate::ops;
use crate::repo::Repo;
use crate::settings::{detect_vaults, expand, Settings};
use anyhow::Result;
use axum::extract::{Path as AxPath, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INDEX_HTML: &str = include_str!("../ui/index.html");

#[derive(Serialize, Clone, Default)]
struct JobInfo {
    kind: String,
    running: bool,
    ok: Option<bool>,
    message: String,
    report: Option<Value>,
    finished_at: Option<String>,
}

struct App {
    token: String,
    port: u16,
    settings: Mutex<Settings>,
    job: Mutex<JobInfo>,
    prog: Progress,
    last_seen: Mutex<Instant>,
}
type S = Arc<App>;
type Res = Result<Json<Value>, (StatusCode, String)>;

fn bad<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

fn start_job<F>(app: &S, kind: &str, f: F) -> Res
where
    F: FnOnce(&Progress) -> Result<Value> + Send + 'static,
{
    {
        let mut j = app.job.lock().unwrap();
        if j.running {
            return Err((StatusCode::CONFLICT, tr!("rs.job_running")));
        }
        *j = JobInfo { kind: kind.into(), running: true, ..Default::default() };
    }
    app.prog.reset(&tr!("phase.starting"));
    let app2 = app.clone();
    std::thread::spawn(move || {
        let r = f(&app2.prog);
        let mut j = app2.job.lock().unwrap();
        j.running = false;
        j.finished_at = Some(chrono::Local::now().to_rfc3339());
        match r {
            Ok(v) => {
                j.ok = Some(true);
                j.report = Some(v);
            }
            Err(e) => {
                j.ok = Some(false);
                j.message = format!("{e:#}");
            }
        }
    });
    Ok(Json(json!({"started": true})))
}

fn do_backup(s: Settings, prog: &Progress) -> Result<Value> {
    if s.vault.is_empty() || s.repo.is_empty() {
        anyhow::bail!(tr!("rs.configure_first"));
    }
    let repo = Repo::open_or_init(&PathBuf::from(&s.repo))?;
    let r = backup::run_backup(&repo, &PathBuf::from(&s.vault), &s.exclude, s.level, prog)?;
    // retention automatica dopo ogni backup riuscito
    if !r.unchanged {
        let _ = ops::prune(&repo, &s.retention, prog);
    }
    Ok(serde_json::to_value(r)?)
}

fn open_repo(app: &S) -> Result<Repo, (StatusCode, String)> {
    let p = app.settings.lock().unwrap().repo.clone();
    if p.is_empty() {
        return Err(bad(tr!("rs.repo_unset")));
    }
    Repo::open(&PathBuf::from(p)).map_err(bad)
}

async fn guard(State(app): State<S>, req: Request, next: Next) -> Response {
    // difesa da DNS rebinding: accetta solo host locali
    let host_ok = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map_or(false, |h| h == format!("127.0.0.1:{}", app.port) || h == format!("localhost:{}", app.port));
    if !host_ok {
        return (StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    if req.uri().path().starts_with("/api/") {
        let tok = req.headers().get("x-token").and_then(|h| h.to_str().ok()).unwrap_or("");
        if tok != app.token {
            return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    }
    next.run(req).await
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn state(State(app): State<S>) -> Json<Value> {
    *app.last_seen.lock().unwrap() = Instant::now();
    let s = app.settings.lock().unwrap().clone();
    let (mut snaps, mut stats, mut err) = (json!([]), Value::Null, Value::Null);
    if !s.repo.is_empty() && Repo::is_repo(&PathBuf::from(&s.repo)) {
        match Repo::open(&PathBuf::from(&s.repo)) {
            Ok(r) => {
                let mut m = r.list_meta().unwrap_or_default();
                m.reverse();
                snaps = serde_json::to_value(m).unwrap();
                stats = serde_json::to_value(r.stats().unwrap_or_default()).unwrap();
            }
            Err(e) => err = json!(e.to_string()),
        }
    }
    let mut job = serde_json::to_value(app.job.lock().unwrap().clone()).unwrap();
    job["progress"] = json!({
        "phase": app.prog.phase.lock().unwrap().clone(),
        "total": app.prog.total.load(Relaxed),
        "done": app.prog.done.load(Relaxed),
        "bytes": app.prog.bytes.load(Relaxed),
        "stored": app.prog.stored.load(Relaxed),
    });
    Json(json!({"version": env!("CARGO_PKG_VERSION"), "settings": s, "snapshots": snaps, "stats": stats, "error": err, "job": job, "detected_vaults": detect_vaults(), "lang": crate::i18n::current(), "langs": crate::i18n::available().iter().map(|(c, n)| json!({"code": c, "name": n})).collect::<Vec<_>>()}))
}

async fn put_settings(State(app): State<S>, Json(mut s): Json<Settings>) -> Res {
    s.vault = expand(&s.vault);
    s.repo = expand(&s.repo);
    backup::build_globset(&s.exclude).map_err(bad)?;
    s.save().map_err(bad)?;
    crate::i18n::set_lang(&s.lang);
    *app.settings.lock().unwrap() = s;
    Ok(Json(json!({"ok": true})))
}

async fn i18n_catalog(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let l = q.get("lang").map(|s| crate::i18n::resolve(s)).unwrap_or_else(crate::i18n::current);
    Json(crate::i18n::catalog_json(&l))
}

async fn run_backup(State(app): State<S>) -> Res {
    let s = app.settings.lock().unwrap().clone();
    start_job(&app, "backup", move |p| do_backup(s, p))
}

#[derive(Deserialize)]
struct RestoreReq {
    id: String,
    dest: String,
}
async fn run_restore(State(app): State<S>, Json(r): Json<RestoreReq>) -> Res {
    let repo = open_repo(&app)?;
    start_job(&app, "restore", move |p| {
        Ok(serde_json::to_value(ops::restore(&repo, &r.id, &PathBuf::from(expand(&r.dest)), p)?)?)
    })
}

async fn run_export(State(app): State<S>, Json(r): Json<RestoreReq>) -> Res {
    let repo = open_repo(&app)?;
    start_job(&app, "export", move |p| {
        let dest = expand(&r.dest);
        let n = ops::export_tar_zst(&repo, &r.id, &PathBuf::from(&dest), p)?;
        Ok(json!({"file": dest, "bytes": n}))
    })
}

#[derive(Deserialize)]
struct VerifyReq {
    full: bool,
}
async fn run_verify(State(app): State<S>, Json(r): Json<VerifyReq>) -> Res {
    let repo = open_repo(&app)?;
    start_job(&app, "verify", move |p| Ok(serde_json::to_value(ops::verify(&repo, r.full, p)?)?))
}

async fn run_prune(State(app): State<S>) -> Res {
    let repo = open_repo(&app)?;
    let ret = app.settings.lock().unwrap().retention.clone();
    start_job(&app, "prune", move |p| Ok(serde_json::to_value(ops::prune(&repo, &ret, p)?)?))
}

async fn del_snapshot(State(app): State<S>, AxPath(id): AxPath<String>) -> Res {
    let repo = open_repo(&app)?;
    start_job(&app, "forget", move |_| Ok(json!({"deleted": ops::forget(&repo, &id)?})))
}

pub fn run(
    port: u16,
    open_browser: bool,
    exit_when_closed: bool,
    url_tx: Option<std::sync::mpsc::Sender<String>>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => l,
            Err(_) => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?,
        };
        let port = listener.local_addr()?.port();
        let token: String = (0..32).map(|_| format!("{:x}", rand::random::<u8>() % 16)).collect();
        let app = Arc::new(App {
            token: token.clone(),
            port,
            settings: Mutex::new(Settings::load()),
            job: Mutex::new(JobInfo::default()),
            prog: Progress::default(),
            last_seen: Mutex::new(Instant::now()),
        });

        // modalità app: esce quando la scheda del browser viene chiusa
        if exit_when_closed {
            let a3 = app.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                let idle = a3.last_seen.lock().unwrap().elapsed();
                if idle > Duration::from_secs(150) && !a3.job.lock().unwrap().running {
                    std::process::exit(0);
                }
            });
        }

        // backup automatico mentre l'app è aperta
        let a2 = app.clone();
        std::thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let s = a2.settings.lock().unwrap().clone();
                if s.auto_minutes == 0 || last.elapsed() < Duration::from_secs(s.auto_minutes as u64 * 60) {
                    continue;
                }
                last = Instant::now();
                let _ = start_job(&a2, "backup", move |p| do_backup(s, p));
            }
        });

        let router = Router::new()
            .route("/", get(index))
            .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
            .route("/api/state", get(state))
            .route("/api/settings", post(put_settings))
            .route("/api/i18n", get(i18n_catalog))
            .route("/api/backup", post(run_backup))
            .route("/api/restore", post(run_restore))
            .route("/api/export", post(run_export))
            .route("/api/verify", post(run_verify))
            .route("/api/prune", post(run_prune))
            .route("/api/snapshots/{id}", delete(del_snapshot))
            .layer(middleware::from_fn_with_state(app.clone(), guard))
            .with_state(app);

        let url = format!("http://127.0.0.1:{port}/#t={token}");
        if let Some(tx) = &url_tx {
            let _ = tx.send(url.clone());
        } else {
            println!("{}", tr!("cli.listening", url = url));
        }
        if open_browser {
            let _ = open::that(&url);
        }
        axum::serve(listener, router).await?;
        Ok(())
    })
}

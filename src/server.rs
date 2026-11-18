use crate::backup::{self, Progress};
use crate::ops;
use crate::backend::LocalBackend;
use crate::copy;
use crate::crypto;
use crate::keychain;
use crate::picker;
use crate::schedule;
use crate::repo::Repo;
use crate::settings::{detect_vaults, expand, Settings};
use anyhow::Result;
use axum::extract::{Path as AxPath, Query, Request, State};
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

/// "none" (nothing there yet), "plain" or "encrypted".
fn repo_kind(path: &str) -> &'static str {
    if path.is_empty() {
        return "none";
    }
    let p = PathBuf::from(path);
    if Repo::is_encrypted_at(&p) {
        "encrypted"
    } else if Repo::is_repo(&p) {
        "plain"
    } else {
        "none"
    }
}

fn check_new_password(p: &str) -> Result<()> {
    if p.chars().count() < 8 {
        anyhow::bail!(tr!("rs.password_short"));
    }
    Ok(())
}

/// Verify a password against the encrypted archive in `path` (without touching the process password).
fn try_unlock(path: &str, pass: &str) -> Result<()> {
    if repo_kind(path) != "encrypted" {
        anyhow::bail!(tr!("rs.not_encrypted"));
    }
    Repo::open_with(Arc::new(LocalBackend::new(&PathBuf::from(path))), path, Some(pass)).map(|_| ())
}

fn create_encrypted(path: &str, pass: &str) -> Result<()> {
    if path.is_empty() {
        anyhow::bail!(tr!("rs.repo_unset"));
    }
    check_new_password(pass)?;
    Repo::init_encrypted(&PathBuf::from(path), pass).map(|_| ())
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
    let kind = repo_kind(&s.repo);
    let mut locked = false;
    if kind != "none" {
        if kind == "encrypted" && crypto::password_for(&PathBuf::from(&s.repo)).is_none() {
            locked = true;
        } else {
            let opened = tokio::task::block_in_place(|| Repo::open(&PathBuf::from(&s.repo)));
            match opened {
                Ok(r) => {
                    let mut m = r.list_meta().unwrap_or_default();
                    m.reverse();
                    snaps = serde_json::to_value(m).unwrap();
                    stats = serde_json::to_value(r.stats().unwrap_or_default()).unwrap();
                }
                // a wrong password from the environment: ask again instead of showing an error
                Err(_) if kind == "encrypted" => locked = true,
                Err(e) => err = json!(e.to_string()),
            }
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
    Json(json!({"version": env!("CARGO_PKG_VERSION"), "settings": s, "snapshots": snaps, "stats": stats, "error": err, "repo_kind": kind, "locked": locked, "picker": picker::supported(), "schedule": schedule::status(), "keychain": {"supported": keychain::supported(), "remembered": kind == "encrypted" && !locked && keychain::load(&keychain::account(&PathBuf::from(&s.repo))).is_some()}, "job": job, "detected_vaults": detect_vaults(), "lang": crate::i18n::current(), "langs": crate::i18n::available().iter().map(|(c, n)| json!({"code": c, "name": n})).collect::<Vec<_>>()}))
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

#[derive(Deserialize)]
struct PassReq {
    password: String,
    /// also keep the password in the system keychain
    #[serde(default)]
    remember: bool,
}

fn remember_if_asked(app: &S, r: &PassReq) -> bool {
    if !r.remember {
        return false;
    }
    let path = app.settings.lock().unwrap().repo.clone();
    keychain::save(&keychain::account(&PathBuf::from(path)), &r.password).is_ok()
}

async fn unlock(State(app): State<S>, Json(r): Json<PassReq>) -> Res {
    let path = app.settings.lock().unwrap().repo.clone();
    let pw = r.password.clone();
    tokio::task::spawn_blocking(move || try_unlock(&path, &pw)).await.map_err(bad)?.map_err(bad)?;
    let remembered = remember_if_asked(&app, &r);
    crypto::set_password(Some(r.password));
    Ok(Json(json!({"ok": true, "remembered": remembered})))
}

async fn encrypt_create(State(app): State<S>, Json(r): Json<PassReq>) -> Res {
    let path = app.settings.lock().unwrap().repo.clone();
    let pw = r.password.clone();
    tokio::task::spawn_blocking(move || create_encrypted(&path, &pw)).await.map_err(bad)?.map_err(bad)?;
    let remembered = remember_if_asked(&app, &r);
    crypto::set_password(Some(r.password));
    Ok(Json(json!({"ok": true, "remembered": remembered})))
}

async fn encrypt_password(State(app): State<S>, Json(r): Json<PassReq>) -> Res {
    check_new_password(&r.password).map_err(bad)?;
    let repo = open_repo(&app)?;
    let pw = r.password.clone();
    tokio::task::spawn_blocking(move || repo.change_password(&pw)).await.map_err(bad)?.map_err(bad)?;
    let remembered = remember_if_asked(&app, &r);
    crypto::set_password(Some(r.password));
    Ok(Json(json!({"ok": true, "remembered": remembered})))
}

#[derive(Deserialize)]
struct KeychainReq {
    remember: bool,
}

/// Save the password currently in use to the keychain, or remove it.
async fn set_keychain(State(app): State<S>, Json(r): Json<KeychainReq>) -> Res {
    let path = PathBuf::from(app.settings.lock().unwrap().repo.clone());
    if !Repo::is_encrypted_at(&path) {
        return Err(bad(tr!("rs.not_encrypted")));
    }
    if r.remember {
        let Some(pw) = crypto::password() else { return Err(bad(tr!("rs.password_required"))) };
        keychain::save(&keychain::account(&path), &pw).map_err(bad)?;
    } else {
        keychain::forget(&keychain::account(&path));
    }
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ScheduleReq {
    enabled: bool,
    minutes: Option<u32>,
}

async fn set_schedule(State(app): State<S>, Json(r): Json<ScheduleReq>) -> Res {
    let s = app.settings.lock().unwrap().clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if !r.enabled {
            return schedule::remove();
        }
        if s.vault.is_empty() || s.repo.is_empty() {
            anyhow::bail!(tr!("rs.schedule_needs_settings"));
        }
        let p = PathBuf::from(&s.repo);
        if Repo::is_encrypted_at(&p) && keychain::load(&keychain::account(&p)).is_none() {
            anyhow::bail!(tr!("rs.schedule_needs_keychain"));
        }
        let minutes = r.minutes.filter(|m| *m > 0).unwrap_or(60).clamp(5, 10_080);
        schedule::install(&std::env::current_exe()?, minutes)
    })
    .await
    .map_err(bad)?
    .map_err(bad)?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct ConvertReq {
    dest: String,
    password: String,
    #[serde(default)]
    remember: bool,
}

/// Create an encrypted copy of the (plain) archive in `dest`, as a background job.
async fn encrypt_convert(State(app): State<S>, Json(r): Json<ConvertReq>) -> Res {
    let s = app.settings.lock().unwrap().clone();
    if repo_kind(&s.repo) != "plain" {
        return Err(bad(tr!("rs.not_plain")));
    }
    check_new_password(&r.password).map_err(bad)?;
    let dest = PathBuf::from(expand(&r.dest));
    if dest.as_os_str().is_empty() {
        return Err(bad(tr!("rs.repo_unset")));
    }
    if std::fs::canonicalize(&s.repo).ok().is_some_and(|a| Some(a) == std::fs::canonicalize(&dest).ok()) {
        return Err(bad(tr!("rs.copy_same")));
    }
    let src = open_repo(&app)?;
    let (level, remember, pw) = (s.level, r.remember, r.password.clone());
    start_job(&app, "copy", move |p| {
        let dst = Repo::init_encrypted(&dest, &pw)?;
        let rep = copy::copy_repo(&src, &dst, level, p)?;
        let v = ops::verify(&dst, false, p)?;
        if !v.ok {
            anyhow::bail!(tr!("res.verify_bad"));
        }
        let remembered = remember && keychain::save(&keychain::account(&dest), &pw).is_ok();
        Ok(json!({"dest": dest.display().to_string(), "copied": rep.copied, "written": rep.written, "remembered": remembered}))
    })
}

#[derive(Deserialize)]
struct PickReq {
    #[serde(default)]
    start: String,
    #[serde(default)]
    prompt: String,
}

/// Open the system "choose a folder" dialog and return the chosen path (null if cancelled).
async fn pick_folder(Json(r): Json<PickReq>) -> Res {
    if !picker::supported() {
        return Err(bad(tr!("rs.picker_unsupported")));
    }
    let start = expand(&r.start);
    let path = tokio::task::spawn_blocking(move || picker::pick_folder(&start, &r.prompt)).await.map_err(bad)?;
    Ok(Json(json!({ "path": path })))
}

async fn run_backup(State(app): State<S>) -> Res {
    let s = app.settings.lock().unwrap().clone();
    start_job(&app, "backup", move |p| do_backup(s, p))
}

#[derive(Deserialize)]
struct RestoreReq {
    id: String,
    dest: String,
    /// only these files or folders (relative to the vault); empty = everything
    #[serde(default)]
    paths: Vec<String>,
}
async fn run_restore(State(app): State<S>, Json(r): Json<RestoreReq>) -> Res {
    let repo = open_repo(&app)?;
    start_job(&app, "restore", move |p| {
        let only = (!r.paths.is_empty()).then_some(r.paths.as_slice());
        Ok(serde_json::to_value(ops::restore_selected(&repo, &r.id, &PathBuf::from(expand(&r.dest)), only, p)?)?)
    })
}

#[derive(Deserialize)]
struct FilesQ {
    q: Option<String>,
    limit: Option<usize>,
}

/// Files of one snapshot, optionally filtered by a text in the path.
async fn snap_files(State(app): State<S>, AxPath(id): AxPath<String>, Query(q): Query<FilesQ>) -> Res {
    let repo = open_repo(&app)?;
    let limit = q.limit.unwrap_or(300).min(2000);
    let query = q.q.unwrap_or_default();
    let v = tokio::task::block_in_place(|| -> Result<Value> {
        let snap = repo.load_snapshot(&id)?;
        let (total, files) = ops::find_files(&snap, &query, limit);
        let files: Vec<Value> = files.iter().map(|e| json!({"path": e.path, "size": e.size, "mtime": e.mtime})).collect();
        Ok(json!({"id": snap.meta.id, "total": total, "files": files}))
    })
    .map_err(bad)?;
    Ok(Json(v))
}

#[derive(Deserialize)]
struct DiffQ {
    base: Option<String>,
}

/// What changed in a snapshot compared with `base` (default: the snapshot just before it).
async fn snap_diff(State(app): State<S>, AxPath(id): AxPath<String>, Query(q): Query<DiffQ>) -> Res {
    let repo = open_repo(&app)?;
    let v = tokio::task::block_in_place(|| -> Result<Value> {
        let id = repo.resolve(&id)?;
        let metas = repo.list_meta()?;
        let base = match q.base {
            Some(b) => Some(repo.resolve(&b)?),
            None => metas.iter().position(|m| m.id == id).filter(|i| *i > 0).map(|i| metas[i - 1].id.clone()),
        };
        let Some(base) = base else {
            return Ok(json!({"id": id, "base": null}));
        };
        let changes = ops::diff_snapshots(&repo.load_snapshot(&base)?, &repo.load_snapshot(&id)?);
        let count = |k: &str| changes.iter().filter(|c| c.kind == k).count();
        let (a, m, r) = (count("added"), count("modified"), count("removed"));
        let total = changes.len();
        let shown: Vec<_> = changes.into_iter().take(1000).collect();
        let created = metas.iter().find(|m| m.id == base).map(|m| m.created.clone());
        Ok(json!({"id": id, "base": base, "base_created": created, "added": a, "modified": m, "removed": r, "total": total, "changes": shown}))
    })
    .map_err(bad)?;
    Ok(Json(v))
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
            .route("/api/unlock", post(unlock))
            .route("/api/keychain", post(set_keychain))
            .route("/api/pick-folder", post(pick_folder))
            .route("/api/encrypt/convert", post(encrypt_convert))
            .route("/api/schedule", post(set_schedule))
            .route("/api/encrypt/create", post(encrypt_create))
            .route("/api/encrypt/password", post(encrypt_password))
            .route("/api/backup", post(run_backup))
            .route("/api/restore", post(run_restore))
            .route("/api/export", post(run_export))
            .route("/api/verify", post(run_verify))
            .route("/api/prune", post(run_prune))
            .route("/api/snapshots/{id}", delete(del_snapshot))
            .route("/api/snapshots/{id}/files", get(snap_files))
            .route("/api/snapshots/{id}/diff", get(snap_diff))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_policy() {
        assert!(check_new_password("short").is_err());
        assert!(check_new_password("long enough").is_ok());
    }

    #[test]
    fn create_unlock_and_kind_flow() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("arch").display().to_string();
        assert_eq!(repo_kind(&p), "none");
        assert_eq!(repo_kind(""), "none");
        assert!(create_encrypted(&p, "short").is_err(), "weak password refused");
        assert_eq!(repo_kind(&p), "none", "nothing was created");
        create_encrypted(&p, "a good password").unwrap();
        assert_eq!(repo_kind(&p), "encrypted");
        assert!(try_unlock(&p, "wrong password").is_err());
        assert!(try_unlock(&p, "a good password").is_ok());
        // an existing archive is never overwritten
        assert!(create_encrypted(&p, "another password").is_err());
        assert!(try_unlock(&p, "a good password").is_ok());
    }

    #[test]
    fn plain_archive_is_reported_and_not_unlockable() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("plain");
        Repo::init(&root).unwrap();
        let p = root.display().to_string();
        assert_eq!(repo_kind(&p), "plain");
        assert!(try_unlock(&p, "whatever1").is_err());
        assert!(create_encrypted(&p, "a good password").is_err());
    }

    #[test]
    fn non_empty_folder_is_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("x.txt"), "x").unwrap();
        assert!(create_encrypted(&d.path().display().to_string(), "a good password").is_err());
    }
}

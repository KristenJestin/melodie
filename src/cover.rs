//! Pochettes locales : `~/.cache/poste/music/covers/<id>.img`, téléchargées
//! une seule fois, en arrière-plan (démon, 512 px). Le lanceur de l'île utilise
//! de petites vignettes (128 px) dans `covers/thumbs/<id>.img`.

use crate::api::{pct_encode, Client};
use crate::db;
use crate::store::write_atomic;
use anyhow::{bail, Context, Result};
use poste_core::Env;
use serde_json::Value;
use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Taille demandée au serveur pour le lecteur.
const SIZE: u32 = 512;
/// Taille des vignettes du lanceur.
const THUMB_SIZE: u32 = 128;
/// Temps total laissé au téléchargement des vignettes manquantes avant de répondre.
pub const BUDGET: Duration = Duration::from_millis(250);
/// Téléchargements simultanés.
const WORKERS: usize = 8;
/// Image « sans pochette » de Navidrome (étoile blanche) : jamais affichée.
const PLACEHOLDER_MD5: &str = "fcec14e686ad5159a8ed7c51936116d7";

/// Nom de fichier sûr pour un identifiant de pochette.
pub fn file_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{safe}.img")
}

pub fn path(env: &Env, id: &str) -> PathBuf {
    db::cache_dir(env).join("covers").join(file_name(id))
}

/// Adresse `file://` d'un chemin absolu (caractères spéciaux encodés).
pub fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy();
    let enc: Vec<String> = s.split('/').map(pct_encode).collect();
    format!("file://{}", enc.join("/"))
}

/// Télécharge la pochette `id` si elle n'est pas déjà là.
pub fn ensure(env: &Env, client: &Client, id: &str) -> Result<PathBuf> {
    let p = path(env, id);
    if p.is_file() {
        return Ok(p);
    }
    let bytes = client
        .fetch(
            "getCoverArt",
            &[("id", id.to_string()), ("size", SIZE.to_string())],
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if bytes.is_empty() {
        bail!("pochette vide");
    }
    write_atomic(&p, &bytes)?;
    Ok(p)
}

pub fn thumb_path(env: &Env, id: &str) -> PathBuf {
    db::cache_dir(env)
        .join("covers")
        .join("thumbs")
        .join(file_name(id))
}

/// Marqueur « cette pochette est l'image par défaut » (pas de nouvel essai).
fn none_path(env: &Env, id: &str) -> PathBuf {
    let mut p = thumb_path(env, id).into_os_string();
    p.push(".none");
    PathBuf::from(p)
}

/// État local d'une pochette pour le lanceur.
enum Local {
    Have(PathBuf),
    /// Image par défaut du serveur : on garde le glyphe.
    Placeholder,
    Missing,
}

/// La vignette si elle existe, sinon la pochette du démon (déjà là, plus grande).
fn local(env: &Env, id: &str) -> Local {
    let t = thumb_path(env, id);
    if t.is_file() {
        return Local::Have(t);
    }
    if none_path(env, id).is_file() {
        return Local::Placeholder;
    }
    let full = path(env, id);
    if full.is_file() {
        return Local::Have(full);
    }
    Local::Missing
}

/// Télécharge la vignette `id` (128 px). L'image par défaut du serveur laisse un marqueur.
fn download_thumb(env: &Env, client: &Client, id: &str) -> Result<()> {
    download_thumb_with(env, client, id, PLACEHOLDER_MD5)
}

fn download_thumb_with(env: &Env, client: &Client, id: &str, placeholder: &str) -> Result<()> {
    let bytes = client
        .fetch(
            "getCoverArt",
            &[("id", id.to_string()), ("size", THUMB_SIZE.to_string())],
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if bytes.is_empty() {
        bail!("pochette vide");
    }
    if format!("{:x}", md5::compute(&bytes)) == placeholder {
        return write_atomic(&none_path(env, id), b"");
    }
    // JPEG, PNG, WebP ou GIF seulement : le reste n'est pas une image.
    if !matches!(bytes[0], 0xff | 0x89 | b'R' | b'G') {
        bail!("pochette illisible");
    }
    write_atomic(&thumb_path(env, id), &bytes)
}

/// Télécharge `ids` en parallèle. Chaque identifiant terminé (réussi ou non) est
/// signalé sur `done` ; l'appelant peut cesser d'attendre quand il veut.
fn spawn_downloads(
    env: &Env,
    client: Arc<Client>,
    ids: Vec<String>,
    done: std::sync::mpsc::Sender<String>,
) {
    let n = ids.len().min(WORKERS);
    let queue = Arc::new(Mutex::new(VecDeque::from(ids)));
    for _ in 0..n {
        let (env, client, queue, done) = (env.clone(), client.clone(), queue.clone(), done.clone());
        std::thread::spawn(move || loop {
            let Some(id) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else {
                break;
            };
            let _ = download_thumb(&env, &client, &id);
            if done.send(id).is_err() {
                break;
            }
        });
    }
}

/// Remplace `_cover` par `icon` (chemin local) dans les entrées du lanceur.
/// Version de production : trousseau lu seulement s'il manque des vignettes,
/// reste du téléchargement confié à un processus détaché.
pub fn apply(env: &Env, entries: &mut [Value]) {
    apply_with(
        env,
        entries,
        BUDGET,
        || crate::state::open_client(env).ok(),
        |ids| {
            if let Err(e) = spawn_background(ids) {
                eprintln!("poste music : pochettes en arrière-plan : {e:#}");
            }
        },
    );
}

/// Cœur de `apply`, avec le client et le lancement du reste injectés (tests).
pub fn apply_with(
    env: &Env,
    entries: &mut [Value],
    budget: Duration,
    open: impl FnOnce() -> Option<Client>,
    background: impl FnOnce(&[String]),
) {
    let mut missing: Vec<String> = Vec::new();
    for e in entries.iter() {
        if let Some(id) = e["_cover"].as_str() {
            if matches!(local(env, id), Local::Missing) && !missing.iter().any(|m| m == id) {
                missing.push(id.to_string());
            }
        }
    }
    if !missing.is_empty() {
        if let Some(client) = open() {
            let start = Instant::now();
            let (tx, rx) = channel();
            spawn_downloads(env, Arc::new(client), missing.clone(), tx);
            let total = missing.len();
            let mut finished = 0;
            while finished < total {
                let left = budget.saturating_sub(start.elapsed());
                match rx.recv_timeout(left) {
                    Ok(id) => {
                        finished += 1;
                        missing.retain(|m| *m != id);
                    }
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // Ce qui reste (en cours ou pas commencé) finira hors budget.
            if !missing.is_empty() {
                background(&missing);
            }
        }
    }
    for e in entries.iter_mut() {
        let Some(obj) = e.as_object_mut() else {
            continue;
        };
        let Some(Value::String(id)) = obj.remove("_cover") else {
            continue;
        };
        if let Local::Have(p) = local(env, &id) {
            obj.insert(
                "icon".into(),
                Value::String(p.to_string_lossy().into_owned()),
            );
        }
    }
}

/// Lance `poste-music cover-fetch` détaché ; les identifiants passent par son
/// entrée standard et le processus relit lui-même le trousseau (aucun jeton
/// dans les arguments ni l'environnement).
fn spawn_background(ids: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("chemin du programme")?;
    let mut c = Command::new(exe);
    c.arg("cover-fetch")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY : `setsid` est sûr entre `fork` et `exec`.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = c.spawn().context("lancement de cover-fetch")?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(ids.join("\n").as_bytes());
    }
    // Pas d'attente : le processus se termine seul.
    Ok(())
}

/// Sous-commande cachée `cover-fetch` : télécharge les vignettes lues sur l'entrée
/// standard. Un seul à la fois (verrou), sinon sortie immédiate : les pochettes
/// manquantes seront redemandées à la frappe suivante.
pub fn fetch_cmd(env: &Env) -> Result<()> {
    let dir = db::cache_dir(env).join("covers");
    std::fs::create_dir_all(&dir).with_context(|| format!("création de {}", dir.display()))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(".fetch.lock"))?;
    // SAFETY : descripteur valide, détenu par `lock`.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(());
    }
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
    let ids: Vec<String> = input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|id| matches!(local(env, id), Local::Missing))
        .map(String::from)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let client = match crate::state::open_client(env) {
        Ok(c) => Arc::new(c),
        Err(_) => return Ok(()),
    };
    let (tx, rx) = channel();
    let n = ids.len();
    spawn_downloads(env, client, ids, tx);
    for _ in 0..n {
        if rx.recv_timeout(Duration::from_secs(30)).is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_et_adresses() {
        assert_eq!(file_name("mf-ab12_65f"), "mf-ab12_65f.img");
        assert_eq!(file_name("../../etc/x y"), "______etc_x_y.img");
        let env = Env::with_home("/home/k");
        assert_eq!(
            path(&env, "al-1"),
            PathBuf::from("/home/k/.cache/poste/music/covers/al-1.img")
        );
        assert_eq!(
            file_url(Path::new("/home/k m/.cache/a.img")),
            "file:///home/k%20m/.cache/a.img"
        );
    }

    use crate::api::{token, Creds};
    use crate::fake::{Fake, PNG};
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    fn setup() -> (tempfile::TempDir, Env, Fake, Creds) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "kris".into(),
            t: token("sesame", "c19b2d"),
            s: "c19b2d".into(),
        };
        let fake = Fake::start(0, &creds.t);
        (d, env, fake, creds)
    }

    fn entries(ids: &[&str]) -> Vec<Value> {
        ids.iter()
            .map(|id| serde_json::json!({"id": *id, "icon": "G", "_cover": *id}))
            .collect()
    }

    #[test]
    fn deja_en_cache_sans_trousseau_ni_reseau() {
        let (_d, env, _fake, _c) = setup();
        write_atomic(&thumb_path(&env, "al-1"), PNG).unwrap();
        // Pochette du démon (512) : réutilisée faute de vignette.
        write_atomic(&path(&env, "al-2"), PNG).unwrap();
        let mut e = entries(&["al-1", "al-2"]);
        let opened = RefCell::new(false);
        apply_with(
            &env,
            &mut e,
            BUDGET,
            || {
                *opened.borrow_mut() = true;
                None
            },
            |_| panic!("rien à télécharger"),
        );
        assert!(!*opened.borrow());
        assert_eq!(
            e[0]["icon"],
            thumb_path(&env, "al-1").to_string_lossy().as_ref()
        );
        assert_eq!(e[1]["icon"], path(&env, "al-2").to_string_lossy().as_ref());
        assert!(e[0].get("_cover").is_none());
    }

    #[test]
    fn telechargement_dans_le_budget() {
        let (_d, env, fake, creds) = setup();
        let client = Client::new(&fake.url, creds);
        let mut e = entries(&["al-1", "pl-1", "ar-1"]);
        apply_with(
            &env,
            &mut e,
            BUDGET * 8,
            || Some(client),
            |_| panic!("tout est arrivé"),
        );
        for x in &e {
            let p = x["icon"].as_str().unwrap();
            assert!(p.contains("/covers/thumbs/"), "{p}");
            assert!(Path::new(p).is_file());
        }
        let calls = fake.state.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c.contains("size=128")));
    }

    #[test]
    fn budget_depasse_garde_le_glyphe_et_delegue_le_reste() {
        let (_d, env, fake, creds) = setup();
        fake.state.cover_delay_ms.store(1500, Ordering::SeqCst);
        let client = Client::new(&fake.url, creds);
        let mut e = entries(&["al-1", "pl-1"]);
        let rest = RefCell::new(Vec::new());
        let t = Instant::now();
        apply_with(
            &env,
            &mut e,
            Duration::from_millis(250),
            || Some(client),
            |ids| rest.borrow_mut().extend(ids.iter().cloned()),
        );
        assert!(
            t.elapsed() < Duration::from_millis(900),
            "{:?}",
            t.elapsed()
        );
        let mut rest = rest.into_inner();
        rest.sort();
        assert_eq!(rest, ["al-1", "pl-1"]);
        for x in &e {
            assert_eq!(x["icon"], "G");
            assert!(x.get("_cover").is_none());
        }
    }

    #[test]
    fn sans_client_le_glyphe_reste() {
        let (_d, env, _f, _c) = setup();
        let mut e = entries(&["al-1"]);
        apply_with(&env, &mut e, BUDGET, || None, |_| panic!("pas de client"));
        assert_eq!(e[0]["icon"], "G");
    }

    #[test]
    fn image_par_defaut_ecartee_sans_nouvel_essai() {
        let (_d, env, fake, creds) = setup();
        let client = Client::new(&fake.url, creds);
        let md5 = format!("{:x}", md5::compute(PNG));
        download_thumb_with(&env, &client, "al-9", &md5).unwrap();
        assert!(!thumb_path(&env, "al-9").exists());
        let mut e = entries(&["al-9"]);
        fake.clear_calls();
        apply_with(&env, &mut e, BUDGET, || panic!("déjà connu"), |_| {});
        assert_eq!(e[0]["icon"], "G");
        assert_eq!(fake.calls_of("getCoverArt"), 0);
    }

    #[test]
    fn jeton_absent_des_sorties() {
        let (_d, env, fake, creds) = setup();
        let secret = creds.t.clone();
        let client = Client::new(&fake.url, creds);
        let mut e = entries(&["al-1"]);
        apply_with(&env, &mut e, BUDGET, || Some(client), |_| {});
        assert!(!Value::Array(e).to_string().contains(&secret));
    }
}

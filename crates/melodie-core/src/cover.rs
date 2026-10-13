//! Pochettes locales : `~/.cache/melodie/covers/<id>.img`, téléchargées
//! une seule fois, en arrière-plan (démon, 512 px). Le lanceur utilise
//! de petites vignettes (128 px) dans `covers/thumbs/<id>.img`. La fenêtre y ajoute son palier
//! de 300 px (`covers/row/<id>.img`) et lit le 512 px du démon pour ses grandes pochettes
//! ([`Tier`]) : le lanceur ne lit que ses vignettes.

use crate::api::{Client, pct_encode};
use crate::db;
use crate::env::Env;
use crate::store::write_atomic;
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Taille demandée au serveur pour le lecteur.
const SIZE: u32 = 512;
/// Taille des vignettes du lanceur.
const THUMB_SIZE: u32 = 128;
/// Taille du palier des grilles et des rangées de la fenêtre.
const ROW_SIZE: u32 = 300;
/// Temps total laissé au téléchargement des vignettes manquantes avant de répondre.
pub const BUDGET: Duration = Duration::from_millis(250);
/// Téléchargements simultanés.
const WORKERS: usize = 8;
/// Code de sortie de `melodie cover-fetch` quand un autre tient le verrou : rien n'a été fait
/// (`EX_TEMPFAIL`). Le lanceur ignore le code ; la fenêtre redemande plus tard.
pub const LOCKED_EXIT: u8 = 75;
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

/// The sizes a cover is kept at. The window draws the smallest one that is not upscaled; the
/// launcher only ever reads `Thumb`, so what the window fetches beside it changes nothing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// The launcher's thumbnail, `covers/thumbs/<id>.img`.
    Thumb,
    /// Rows and grids of the window, `covers/row/<id>.img`.
    Row,
    /// Large covers: the daemon's own file, `covers/<id>.img` (the player, the highlight on a
    /// screen with a high scale factor).
    Large,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Thumb, Tier::Row, Tier::Large];

    /// The size asked from the server, in pixels.
    pub fn size(self) -> u32 {
        match self {
            Tier::Thumb => THUMB_SIZE,
            Tier::Row => ROW_SIZE,
            Tier::Large => SIZE,
        }
    }

    pub fn from_size(size: u32) -> Option<Tier> {
        Tier::ALL.into_iter().find(|t| t.size() == size)
    }

    /// The smallest tier with at least the pixels of a cover drawn `side` logical pixels wide on
    /// a screen of that scale factor; the largest when none is enough.
    pub fn for_draw(side: f32, scale: f32) -> Tier {
        let need = (side * scale).ceil().max(0.0) as u32;
        Tier::ALL
            .into_iter()
            .find(|t| t.size() >= need)
            .unwrap_or(Tier::Large)
    }
}

/// Where the cover `id` is kept at `tier`.
pub fn tier_path(env: &Env, id: &str, tier: Tier) -> PathBuf {
    match tier {
        Tier::Thumb => thumb_path(env, id),
        Tier::Row => db::cache_dir(env)
            .join("covers")
            .join("row")
            .join(file_name(id)),
        Tier::Large => path(env, id),
    }
}

/// What is on disk for one cover, at every tier.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Disk {
    files: [Option<PathBuf>; 3],
    marker: bool,
}

impl Disk {
    /// Checks files: run it away from the window's thread.
    pub fn read(env: &Env, id: &str) -> Self {
        let marker = none_path(env, id).is_file();
        // The marker wins: the daemon's `ensure` has no md5 check, so for a cover that is the
        // server's default image its file is that image, beside the marker.
        let file = |t| Some(tier_path(env, id, t)).filter(|p| !marker && p.is_file());
        Self {
            files: Tier::ALL.map(file),
            marker,
        }
    }

    /// The server's default image was found for this cover: no file at any tier, no new try.
    pub fn placeholder(&self) -> bool {
        self.marker
    }

    /// The file for a cover drawn from `want`: the smallest tier that is not smaller, else the
    /// largest below (shown until the right one lands).
    pub fn pick(&self, want: Tier) -> Option<&PathBuf> {
        let (below, from) = self.files.split_at(want as usize);
        from.iter()
            .chain(below.iter().rev())
            .find_map(Option::as_ref)
    }

    /// Whether a file as large as `want` is there.
    pub fn has(&self, want: Tier) -> bool {
        self.files[want as usize..].iter().any(Option::is_some)
    }

    /// The tier `pick` gives for `want`.
    pub fn tier_of(&self, want: Tier) -> Option<Tier> {
        let (below, from) = self.files.split_at(want as usize);
        Tier::ALL[want as usize..]
            .iter()
            .zip(from)
            .chain(
                Tier::ALL[..want as usize]
                    .iter()
                    .rev()
                    .zip(below.iter().rev()),
            )
            .find_map(|(t, f)| f.as_ref().map(|_| *t))
    }

    /// The file of this tier, if it is there.
    pub fn at(&self, tier: Tier) -> Option<&PathBuf> {
        self.files[tier as usize].as_ref()
    }

    /// The cheapest file to read (tinting a cover does not need a large one).
    pub fn smallest(&self) -> Option<&PathBuf> {
        self.files.iter().find_map(Option::as_ref)
    }
}

/// One cover wanted at one tier: a line of `cover-fetch`'s input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Want {
    pub id: String,
    pub tier: Tier,
}

impl Want {
    pub fn new(id: impl Into<String>, tier: Tier) -> Self {
        Self {
            id: id.into(),
            tier,
        }
    }

    /// The thumbnail is the bare id, as the launcher has always written it; the other tiers add
    /// their size after a tab.
    pub fn line(&self) -> String {
        match self.tier {
            Tier::Thumb => self.id.clone(),
            tier => format!("{}\t{}", self.id, tier.size()),
        }
    }
}

/// Reads the input of `cover-fetch`. A size that is no tier is ignored: the server is only asked
/// for the sizes the window and the launcher use.
pub fn parse_wants(input: &str) -> Vec<Want> {
    input
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(|l| match l.split_once('\t') {
            None => Some(Want::new(l, Tier::Thumb)),
            Some((id, size)) => {
                let tier = Tier::from_size(size.trim().parse().ok()?)?;
                Some(Want::new(id.trim(), tier))
            }
        })
        .collect()
}

/// Marqueur « cette pochette est l'image par défaut » (pas de nouvel essai).
fn none_path(env: &Env, id: &str) -> PathBuf {
    let mut p = thumb_path(env, id).into_os_string();
    p.push(".none");
    PathBuf::from(p)
}

/// État local d'une pochette pour le lanceur.
pub enum Local {
    Have(PathBuf),
    /// Image par défaut du serveur : on garde le glyphe.
    Placeholder,
    Missing,
}

/// La vignette si elle existe, sinon la pochette du démon (déjà là, plus grande).
pub fn local(env: &Env, id: &str) -> Local {
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
    download(env, client, id, Tier::Thumb)
}

/// Downloads the cover `id` at `tier`.
pub fn download(env: &Env, client: &Client, id: &str, tier: Tier) -> Result<()> {
    download_with(env, client, id, tier, PLACEHOLDER_MD5)
}

fn download_with(
    env: &Env,
    client: &Client,
    id: &str,
    tier: Tier,
    placeholder: &str,
) -> Result<()> {
    // The default image is known by its bytes at the thumbnail's size only: a cover not seen yet
    // goes through its thumbnail first, whose marker then holds for every tier.
    if tier != Tier::Thumb {
        if !thumb_path(env, id).is_file() && !none_path(env, id).is_file() {
            download_with(env, client, id, Tier::Thumb, placeholder)?;
        }
        if none_path(env, id).is_file() {
            return Ok(());
        }
    }
    let bytes = client
        .fetch(
            "getCoverArt",
            &[("id", id.to_string()), ("size", tier.size().to_string())],
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if bytes.is_empty() {
        bail!("pochette vide");
    }
    if tier == Tier::Thumb && format!("{:x}", md5::compute(&bytes)) == placeholder {
        return write_atomic(&none_path(env, id), b"");
    }
    // JPEG, PNG, WebP ou GIF seulement : le reste n'est pas une image.
    if !matches!(bytes[0], 0xff | 0x89 | b'R' | b'G') {
        bail!("pochette illisible");
    }
    write_atomic(&tier_path(env, id, tier), &bytes)
}

/// Télécharge `ids` en parallèle. Chaque identifiant terminé (réussi ou non) est
/// signalé sur `done` ; l'appelant peut cesser d'attendre quand il veut.
pub fn spawn_downloads(
    env: &Env,
    client: Arc<Client>,
    ids: Vec<String>,
    done: std::sync::mpsc::Sender<String>,
) {
    pool(env, client, ids, done, |env, client, id, done| {
        let _ = download_thumb(env, client, &id);
        done.send(id).is_ok()
    });
}

/// The same for covers at any tier. The tiers of one cover are fetched one after the other by
/// the same worker: each starts from the thumbnail (see [`download`]), which two workers would
/// write at once.
pub fn spawn_wants(
    env: &Env,
    client: Arc<Client>,
    wants: Vec<Want>,
    done: std::sync::mpsc::Sender<Want>,
) {
    let mut by_id: Vec<Vec<Want>> = Vec::new();
    for w in wants {
        match by_id.iter_mut().find(|g| g[0].id == w.id) {
            Some(group) => group.push(w),
            None => by_id.push(vec![w]),
        }
    }
    pool(env, client, by_id, done, |env, client, group, done| {
        for w in group {
            let _ = download(env, client, &w.id, w.tier);
            if done.send(w).is_err() {
                return false;
            }
        }
        true
    });
}

/// Runs `work` on the items with up to `WORKERS` threads; `work` says whether to go on.
fn pool<T: Send + 'static, D: Send + 'static>(
    env: &Env,
    client: Arc<Client>,
    items: Vec<T>,
    done: std::sync::mpsc::Sender<D>,
    work: fn(&Env, &Client, T, &std::sync::mpsc::Sender<D>) -> bool,
) {
    let n = items.len().min(WORKERS);
    let queue = Arc::new(Mutex::new(VecDeque::from(items)));
    for _ in 0..n {
        let (env, client, queue, done) = (env.clone(), client.clone(), queue.clone(), done.clone());
        std::thread::spawn(move || {
            loop {
                let Some(item) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else {
                    break;
                };
                if !work(&env, &client, item, &done) {
                    break;
                }
            }
        });
    }
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
        if let Some(id) = e["_cover"].as_str()
            && matches!(local(env, id), Local::Missing)
            && !missing.iter().any(|m| m == id)
        {
            missing.push(id.to_string());
        }
    }
    if !missing.is_empty()
        && let Some(client) = open()
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noms_et_adresses() {
        assert_eq!(file_name("mf-ab12_65f"), "mf-ab12_65f.img");
        assert_eq!(file_name("../../etc/x y"), "______etc_x_y.img");
        let env = Env::with_home("/home/alice");
        assert_eq!(
            path(&env, "al-1"),
            PathBuf::from("/home/alice/.cache/melodie/covers/al-1.img")
        );
        assert_eq!(
            file_url(Path::new("/home/alice m/.cache/a.img")),
            "file:///home/alice%20m/.cache/a.img"
        );
    }

    use crate::api::{Creds, token};
    use crate::fake::{Fake, PNG};
    use std::cell::RefCell;
    use std::sync::atomic::Ordering;

    fn setup() -> (tempfile::TempDir, Env, Fake, Creds) {
        let d = tempfile::tempdir().unwrap();
        let env = Env::with_home(d.path());
        let creds = Creds {
            u: "alice".into(),
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
        download_with(&env, &client, "al-9", Tier::Thumb, &md5).unwrap();
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

    fn tier_files(env: &Env, id: &str, tiers: &[Tier]) {
        for t in tiers {
            write_atomic(&tier_path(env, id, *t), PNG).unwrap();
        }
    }

    #[test]
    fn a_tier_is_the_smallest_one_that_is_not_upscaled() {
        // 168 px at scale 1, 212 px at scale 1: the window's row and card
        assert_eq!(Tier::for_draw(40., 1.), Tier::Thumb);
        assert_eq!(Tier::for_draw(128., 1.), Tier::Thumb);
        assert_eq!(Tier::for_draw(168., 1.), Tier::Row);
        assert_eq!(Tier::for_draw(212., 1.), Tier::Row);
        assert_eq!(Tier::for_draw(300., 1.), Tier::Row);
        // a hi-dpi screen needs twice the pixels
        assert_eq!(Tier::for_draw(168., 2.), Tier::Large);
        assert_eq!(Tier::for_draw(40., 2.), Tier::Thumb);
        assert_eq!(
            Tier::for_draw(40., 3.5),
            Tier::Row,
            "140 px: 128 is not enough"
        );
        // nothing above the largest
        assert_eq!(Tier::for_draw(900., 2.), Tier::Large);
    }

    #[test]
    fn the_launchers_files_are_the_thumbnails_and_the_daemons_covers() {
        let env = Env::with_home("/home/alice");
        assert_eq!(
            tier_path(&env, "al-1", Tier::Thumb),
            thumb_path(&env, "al-1")
        );
        assert_eq!(tier_path(&env, "al-1", Tier::Large), path(&env, "al-1"));
        let row = tier_path(&env, "al-1", Tier::Row);
        assert_ne!(row, thumb_path(&env, "al-1"));
        assert_ne!(row, path(&env, "al-1"));
        assert!(row.starts_with("/home/alice/.cache/melodie/covers/"));
        assert_eq!(Tier::Thumb.size(), THUMB_SIZE);
        assert_eq!(Tier::Large.size(), SIZE);
    }

    #[test]
    fn what_is_on_disk_is_picked_without_upscaling_when_it_can_be() {
        let (_d, env, _fake, _c) = setup();
        let none = Disk::read(&env, "al-1");
        assert_eq!(none.pick(Tier::Row), None);
        tier_files(&env, "al-1", &[Tier::Thumb]);
        let disk = Disk::read(&env, "al-1");
        assert_eq!(
            disk.pick(Tier::Row),
            Some(&thumb_path(&env, "al-1")),
            "the small one meanwhile"
        );
        assert!(!disk.has(Tier::Row));
        tier_files(&env, "al-1", &[Tier::Large, Tier::Row]);
        let disk = Disk::read(&env, "al-1");
        assert_eq!(disk.pick(Tier::Thumb), Some(&thumb_path(&env, "al-1")));
        assert_eq!(
            disk.pick(Tier::Row),
            Some(&tier_path(&env, "al-1", Tier::Row))
        );
        assert_eq!(disk.pick(Tier::Large), Some(&path(&env, "al-1")));
        assert!(disk.has(Tier::Row));
        // only the large one: it serves a row (downscaled), and the thumbnail wish
        tier_files(&env, "al-2", &[Tier::Large]);
        let disk = Disk::read(&env, "al-2");
        assert_eq!(disk.pick(Tier::Row), Some(&path(&env, "al-2")));
        assert!(disk.has(Tier::Thumb));
    }

    #[test]
    fn the_launcher_still_reads_its_thumbnail_whatever_the_window_fetched() {
        let (_d, env, _fake, _c) = setup();
        tier_files(&env, "al-1", &[Tier::Row]);
        assert!(
            matches!(local(&env, "al-1"), Local::Missing),
            "the launcher only knows its thumbnail and the daemon's cover"
        );
        tier_files(&env, "al-1", &[Tier::Thumb]);
        assert!(matches!(local(&env, "al-1"), Local::Have(p) if p == thumb_path(&env, "al-1")));
    }

    #[test]
    fn the_window_asks_the_server_for_the_size_of_the_tier() {
        let (_d, env, fake, creds) = setup();
        let client = Client::new(&fake.url, creds);
        download(&env, &client, "al-1", Tier::Row).unwrap();
        download(&env, &client, "al-1", Tier::Large).unwrap();
        assert!(tier_path(&env, "al-1", Tier::Row).is_file());
        assert!(tier_path(&env, "al-1", Tier::Large).is_file());
        let calls = fake.state.calls.lock().unwrap().clone();
        assert!(calls.iter().any(|c| c.contains("size=300")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("size=512")), "{calls:?}");
    }

    #[test]
    fn a_larger_tier_checks_the_default_image_on_the_thumbnail_first() {
        // the default image has the same bytes only at one size: the thumbnail is the judge
        let (_d, env, fake, creds) = setup();
        let client = Client::new(&fake.url, creds);
        let md5 = format!("{:x}", md5::compute(PNG));
        download_with(&env, &client, "al-9", Tier::Row, &md5).unwrap();
        assert!(!tier_path(&env, "al-9", Tier::Row).exists());
        assert!(!thumb_path(&env, "al-9").exists());
        assert!(none_path(&env, "al-9").is_file(), "the marker is per id");
        fake.clear_calls();
        download_with(&env, &client, "al-9", Tier::Large, &md5).unwrap();
        assert_eq!(
            fake.calls_of("getCoverArt"),
            0,
            "not asked again, at any size"
        );
        assert!(matches!(local(&env, "al-9"), Local::Placeholder));
        assert!(Disk::read(&env, "al-9").placeholder());
    }

    #[test]
    fn a_placeholder_marker_hides_the_daemons_file() {
        // the daemon's `ensure` has no md5 check: for an album without art it writes the server's
        // default image, beside the marker the thumbnail fetch left
        let (_d, env, _fake, _c) = setup();
        write_atomic(&none_path(&env, "al-9"), b"").unwrap();
        tier_files(&env, "al-9", &[Tier::Large]);
        let disk = Disk::read(&env, "al-9");
        assert!(disk.placeholder());
        for t in Tier::ALL {
            assert_eq!(disk.pick(t), None, "{t:?}");
            assert!(!disk.has(t), "{t:?}");
        }
        assert_eq!(disk.smallest(), None);
        assert!(matches!(local(&env, "al-9"), Local::Placeholder));
    }

    #[test]
    fn a_known_thumbnail_is_not_fetched_again_for_a_larger_tier() {
        let (_d, env, fake, creds) = setup();
        let client = Client::new(&fake.url, creds);
        tier_files(&env, "al-1", &[Tier::Thumb]);
        download(&env, &client, "al-1", Tier::Row).unwrap();
        let calls = fake.state.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].contains("size=300"));
    }

    #[test]
    fn the_input_of_cover_fetch_keeps_the_launchers_lines_as_they_are() {
        let wants =
            parse_wants("al-1\n\n  al-2  \nal-3\t300\nal-4\t512\nal-5\t128\nal-6\t77\nal-7\tx\n");
        let got: Vec<(&str, Tier)> = wants.iter().map(|w| (w.id.as_str(), w.tier)).collect();
        assert_eq!(
            got,
            [
                ("al-1", Tier::Thumb),
                ("al-2", Tier::Thumb),
                ("al-3", Tier::Row),
                ("al-4", Tier::Large),
                ("al-5", Tier::Thumb),
            ],
            "no size is the thumbnail; a size that is no tier is ignored"
        );
        let lines: Vec<String> = wants.iter().map(Want::line).collect();
        assert_eq!(lines[0], "al-1", "the launcher's line is the bare id");
        assert_eq!(lines[2], "al-3\t300");
        assert_eq!(parse_wants(&lines.join("\n")).len(), wants.len());
    }

    #[test]
    fn two_tiers_of_one_cover_never_write_its_thumbnail_at_once() {
        // a regression: two workers both fetched the thumbnail first and wrote it at once, and one
        // lost the rename (they shared a temporary file)
        for round in 0..5 {
            let (_d, env, fake, creds) = setup();
            // both thumbnail requests are answered together, so that both writes meet
            fake.state.cover_delay_ms.store(30, Ordering::SeqCst);
            let client = Arc::new(Client::new(&fake.url, creds));
            let (tx, rx) = channel();
            let id = format!("al-{round}");
            let wants = vec![Want::new(&id, Tier::Row), Want::new(&id, Tier::Large)];
            spawn_wants(&env, client, wants, tx);
            for _ in 0..2 {
                rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            assert!(tier_path(&env, &id, Tier::Row).is_file(), "round {round}");
            assert!(tier_path(&env, &id, Tier::Large).is_file(), "round {round}");
            // one worker, one thumbnail request: nothing else could have written it
            let calls = fake.state.calls.lock().unwrap().clone();
            let thumbs = calls.iter().filter(|c| c.contains("size=128")).count();
            assert_eq!(thumbs, 1, "round {round}: {calls:?}");
        }
    }

    #[test]
    fn tiers_download_in_parallel_and_each_is_told_when_done() {
        let (_d, env, fake, creds) = setup();
        let client = Arc::new(Client::new(&fake.url, creds));
        let (tx, rx) = channel();
        let wants = vec![
            Want::new("al-1", Tier::Row),
            Want::new("al-1", Tier::Large),
            Want::new("pl-1", Tier::Thumb),
        ];
        spawn_wants(&env, client, wants.clone(), tx);
        let mut done: Vec<Want> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        done.sort_by_key(|w| (w.id.clone(), w.tier));
        let mut expected = wants;
        expected.sort_by_key(|w| (w.id.clone(), w.tier));
        assert_eq!(done, expected);
        assert!(tier_path(&env, "al-1", Tier::Row).is_file());
        assert!(tier_path(&env, "al-1", Tier::Large).is_file());
        assert!(thumb_path(&env, "pl-1").is_file());
    }
}

//! Client Subsonic/OpenSubsonic (Navidrome) : authentification par jeton,
//! requêtes bloquantes, classement des erreurs.
//!
//! Le jeton ne doit jamais apparaître dans un message : les erreurs de `ureq`
//! contiennent l'URL complète, donc elles ne sont jamais affichées telles quelles.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::time::Duration;

const API_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "poste-music";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_BODY: u64 = 64 * 1024 * 1024;
const MAX_IMAGE: u64 = 16 * 1024 * 1024;

/// Authentification Subsonic par jeton : le mot de passe n'y figure pas.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Creds {
    /// Utilisateur.
    pub u: String,
    /// Jeton : md5(motdepasse + sel), hexadécimal minuscule.
    pub t: String,
    /// Sel.
    pub s: String,
}

impl fmt::Debug for Creds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Creds {{ u: {:?}, t: <masqué>, s: <masqué> }}", self.u)
    }
}

impl fmt::Display for Creds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.u)
    }
}

/// `md5(motdepasse + sel)` en hexadécimal minuscule.
pub fn token(password: &str, salt: &str) -> String {
    let mut data = String::with_capacity(password.len() + salt.len());
    data.push_str(password);
    data.push_str(salt);
    let t = format!("{:x}", md5::compute(data.as_bytes()));
    data.clear();
    t
}

/// Sel aléatoire de `len` caractères alphanumériques (tirage sans biais).
pub fn random_salt(len: usize) -> anyhow::Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let mut buf = [0u8; 32];
        getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("aléa indisponible : {e}"))?;
        for b in buf {
            // 248 = 4 * 62 : on rejette le reste pour ne pas biaiser.
            if b < 248 && out.len() < len {
                out.push(ALPHABET[(b % 62) as usize] as char);
            }
        }
    }
    Ok(out)
}

impl Creds {
    /// Calcule les identifiants à partir du mot de passe, qui n'est pas conservé.
    pub fn from_password(user: &str, password: &str) -> anyhow::Result<Self> {
        let s = random_salt(16)?;
        Ok(Self {
            u: user.to_string(),
            t: token(password, &s),
            s,
        })
    }
}

/// Erreur d'appel, déjà classée et nettoyée (aucun jeton dedans).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Le serveur refuse le jeton (codes Subsonic 40 à 44).
    AuthRefused(String),
    /// Panne réseau, délai dépassé, HTTP non 200, JSON invalide, ou erreur
    /// Subsonic sans rapport avec l'authentification.
    Unreachable(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::AuthRefused(m) => write!(f, "authentification refusée : {m}"),
            ApiError::Unreachable(m) => write!(f, "serveur injoignable : {m}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// Une requête prête à partir. Son affichage (`Display`, `Debug`) ne montre que
/// la méthode ; l'URL complète n'est accessible que par `full_url`.
pub struct Request {
    method: String,
    base: String,
    params: Vec<(String, String)>,
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.method)
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Request({})", self.method)
    }
}

impl Request {
    /// URL de base sans paramètres : `<url>/rest/<méthode>.view`.
    pub fn endpoint(&self) -> String {
        format!("{}/rest/{}.view", self.base, self.method)
    }

    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    /// URL complète avec le jeton. À ne jamais afficher ni journaliser.
    #[cfg(test)]
    pub fn full_url(&self) -> String {
        let q: Vec<String> = self
            .params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        format!("{}?{}", self.endpoint(), q.join("&"))
    }
}

/// URL d'un flux, jeton compris : jamais affichée ni journalisée (`Display` et
/// `Debug` ne montrent rien), seul `expose` donne le texte, pour mpv.
pub struct SecretUrl(String);

impl SecretUrl {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<url masquée>")
    }
}

impl fmt::Debug for SecretUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretUrl(<masquée>)")
    }
}

/// Encodage pourcentage de tout sauf les caractères non réservés.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Client d'un serveur, avec ses identifiants.
#[derive(Clone)]
pub struct Client {
    base: String,
    creds: Creds,
    agent: ureq::Agent,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Client {{ url: {:?}, user: {:?} }}",
            self.base, self.creds.u
        )
    }
}

/// Retire les barres finales d'une adresse de serveur.
pub fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

impl Client {
    pub fn new(url: &str, creds: Creds) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .user_agent(CLIENT_NAME)
            .build();
        Self {
            base: normalize_url(url),
            creds,
            agent,
        }
    }

    pub fn url(&self) -> &str {
        &self.base
    }

    pub fn request(&self, method: &str, extra: &[(&str, String)]) -> Request {
        let mut params = vec![
            ("u".to_string(), self.creds.u.clone()),
            ("t".to_string(), self.creds.t.clone()),
            ("s".to_string(), self.creds.s.clone()),
            ("v".to_string(), API_VERSION.to_string()),
            ("c".to_string(), CLIENT_NAME.to_string()),
            ("f".to_string(), "json".to_string()),
        ];
        params.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        Request {
            method: method.to_string(),
            base: self.base.clone(),
            params,
        }
    }

    /// Envoie une requête et renvoie la réponse HTTP (statut 200 vérifié).
    fn send(&self, method: &str, extra: &[(&str, String)]) -> Result<ureq::Response, ApiError> {
        let req = self.request(method, extra);
        let mut r = self.agent.get(&req.endpoint());
        for (k, v) in req.params() {
            r = r.query(k, v);
        }
        let resp = match r.call() {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, _)) => {
                return Err(ApiError::Unreachable(format!("{method} : HTTP {code}")))
            }
            Err(ureq::Error::Transport(t)) => {
                // `Display` de `Transport` contient l'URL : on n'en garde que le genre.
                let detail = format!("{method} : {}", t.kind());
                return Err(ApiError::Unreachable(self.scrub(&detail)));
            }
        };
        if resp.status() != 200 {
            return Err(ApiError::Unreachable(format!(
                "{method} : HTTP {}",
                resp.status()
            )));
        }
        Ok(resp)
    }

    fn read_body(&self, method: &str, resp: ureq::Response, max: u64) -> Result<Vec<u8>, ApiError> {
        let mut body = Vec::new();
        resp.into_reader()
            .take(max)
            .read_to_end(&mut body)
            .map_err(|e| ApiError::Unreachable(self.scrub(&format!("{method} : lecture : {e}"))))?;
        Ok(body)
    }

    /// Appelle une méthode et renvoie le contenu de `subsonic-response`.
    pub fn call(&self, method: &str, extra: &[(&str, String)]) -> Result<Value, ApiError> {
        let resp = self.send(method, extra)?;
        let body = self.read_body(method, resp, MAX_BODY)?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| ApiError::Unreachable(format!("{method} : réponse JSON invalide")))?;
        parse_response(method, value)
    }

    /// Télécharge un contenu binaire (pochette). Une réponse JSON est une erreur Subsonic.
    pub fn fetch(&self, method: &str, extra: &[(&str, String)]) -> Result<Vec<u8>, ApiError> {
        let resp = self.send(method, extra)?;
        let body = self.read_body(method, resp, MAX_IMAGE)?;
        if body.first() == Some(&b'{') {
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                return parse_response(method, v).map(|_| Vec::new());
            }
        }
        Ok(body)
    }

    /// Adresse de lecture d'un morceau, pour mpv seulement.
    pub fn stream_url(&self, id: &str) -> SecretUrl {
        SecretUrl(format!(
            "{}/rest/stream.view?id={}&format=raw&u={}&t={}&s={}&v={API_VERSION}&c={CLIENT_NAME}",
            self.base,
            pct_encode(id),
            pct_encode(&self.creds.u),
            self.creds.t,
            self.creds.s
        ))
    }

    /// Retire de `s` tout ce qui ressemble au jeton ou au sel.
    fn scrub(&self, s: &str) -> String {
        scrub(s, &self.creds)
    }

    /// `ping.view` : vérifie l'adresse et les identifiants.
    pub fn ping(&self) -> Result<(), ApiError> {
        self.call("ping", &[]).map(|_| ())
    }
}

fn scrub(s: &str, c: &Creds) -> String {
    let mut out = s.to_string();
    for secret in [&c.t, &c.s] {
        if !secret.is_empty() {
            out = out.replace(secret.as_str(), "<masqué>");
        }
    }
    out
}

/// Classe une réponse JSON Subsonic.
pub fn parse_response(method: &str, value: Value) -> Result<Value, ApiError> {
    let Some(resp) = value.get("subsonic-response") else {
        return Err(ApiError::Unreachable(format!(
            "{method} : réponse sans subsonic-response"
        )));
    };
    match resp.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(resp.clone()),
        Some("failed") => {
            let code = resp["error"]["code"].as_i64().unwrap_or(0);
            let msg = resp["error"]["message"].as_str().unwrap_or("sans message");
            Err(classify_failure(method, code, msg))
        }
        _ => Err(ApiError::Unreachable(format!(
            "{method} : statut absent ou inconnu"
        ))),
    }
}

/// Codes 40 à 44 : jeton, nom ou mot de passe refusés.
pub fn classify_failure(method: &str, code: i64, msg: &str) -> ApiError {
    match code {
        40..=44 => ApiError::AuthRefused(format!("{method} : code {code} ({msg})")),
        _ => ApiError::Unreachable(format!("{method} : erreur Subsonic {code} ({msg})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jeton_connu() {
        assert_eq!(
            token("sesame", "c19b2d"),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn sel_alphanumerique() {
        let a = random_salt(16).unwrap();
        let b = random_salt(16).unwrap();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
    }

    #[test]
    fn creds_depuis_mot_de_passe() {
        let c = Creds::from_password("kris", "sesame").unwrap();
        assert!(c.s.len() >= 12);
        assert_eq!(c.t, token("sesame", &c.s));
        assert_eq!(c.t.len(), 32);
        assert!(!format!("{c:?}{c}").contains("sesame"));
    }

    #[test]
    fn codes_d_authentification() {
        for code in 40..=44 {
            assert!(matches!(
                classify_failure("ping", code, "x"),
                ApiError::AuthRefused(_)
            ));
        }
        for code in [0, 10, 20, 30, 39, 45, 50, 70] {
            assert!(matches!(
                classify_failure("ping", code, "x"),
                ApiError::Unreachable(_)
            ));
        }
    }

    #[test]
    fn reponses_classees() {
        let ok = json!({"subsonic-response": {"status": "ok", "version": "1.16.1"}});
        assert!(parse_response("ping", ok).is_ok());
        let ko = json!({"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "Wrong"}}});
        assert!(matches!(
            parse_response("ping", ko),
            Err(ApiError::AuthRefused(_))
        ));
        assert!(matches!(
            parse_response("ping", json!({"x": 1})),
            Err(ApiError::Unreachable(_))
        ));
    }

    #[test]
    fn requete_sans_fuite() {
        let creds = Creds::from_password("kris", "sesame").unwrap();
        let client = Client::new("http://h:4533/", creds.clone());
        let req = client.request("search3", &[("query", String::new())]);
        assert!(req.full_url().contains(&creds.t));
        assert!(req
            .full_url()
            .starts_with("http://h:4533/rest/search3.view?u=kris&t="));
        assert!(req
            .full_url()
            .contains("&v=1.16.1&c=poste-music&f=json&query="));
        for text in [
            format!("{req}"),
            format!("{req:?}"),
            format!("{client:?}"),
            format!("{creds:?}"),
            format!("{creds}"),
        ] {
            assert!(!text.contains(&creds.t), "fuite du jeton : {text}");
            assert!(!text.contains(&creds.s), "fuite du sel : {text}");
        }
    }

    #[test]
    fn url_de_flux_sans_fuite() {
        let creds = Creds::from_password("kr is", "sesame").unwrap();
        let client = Client::new("http://h:4533/", creds.clone());
        let url = client.stream_url("a b&c");
        let raw = url.expose();
        assert!(
            raw.starts_with("http://h:4533/rest/stream.view?id=a%20b%26c&format=raw&u=kr%20is&t=")
        );
        assert!(raw.contains(&creds.t) && raw.contains(&creds.s));
        assert!(raw.ends_with("&v=1.16.1&c=poste-music"));
        for text in [format!("{url}"), format!("{url:?}")] {
            assert!(!text.contains(&creds.t) && !text.contains(&creds.s));
            assert!(!text.contains("stream.view"));
        }
    }

    #[test]
    fn nettoyage_des_messages() {
        let c = Creds {
            u: "k".into(),
            t: "abcdef0123".into(),
            s: "selsel".into(),
        };
        assert_eq!(
            scrub("GET /x?t=abcdef0123&s=selsel", &c),
            "GET /x?t=<masqué>&s=<masqué>"
        );
    }
}

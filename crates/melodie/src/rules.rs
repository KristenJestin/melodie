//! Règles de lecture sans effet de bord : écoute à signaler, morceau passé,
//! mélange.

/// Au plus 4 minutes écoutées avant de signaler l'écoute.
pub const SCROBBLE_MAX_SECS: f64 = 240.0;
/// Un morceau passé avant 30 s compte comme « passé ».
pub const SKIP_MAX_SECS: f64 = 30.0;

/// Temps écouté à partir duquel l'écoute est signalée : 50 % de la durée ou
/// 4 minutes, le premier atteint. Durée inconnue (0) : 4 minutes.
pub fn scrobble_threshold(duration: f64) -> f64 {
    if duration > 0.0 {
        (duration / 2.0).min(SCROBBLE_MAX_SECS)
    } else {
        SCROBBLE_MAX_SECS
    }
}

/// Temps réellement joué d'un morceau. Les pauses ne comptent pas.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrobbleClock {
    threshold: f64,
    played: f64,
    submitted: bool,
}

impl ScrobbleClock {
    pub fn new(duration: f64) -> Self {
        Self {
            threshold: scrobble_threshold(duration),
            played: 0.0,
            submitted: false,
        }
    }

    #[cfg(test)]
    pub fn played(&self) -> f64 {
        self.played
    }

    /// Ajoute `dt` secondes si la lecture avance (`playing`). Renvoie `true`
    /// une seule fois : au moment où le seuil est atteint.
    pub fn advance(&mut self, dt: f64, playing: bool) -> bool {
        if !playing || dt <= 0.0 {
            return false;
        }
        self.played += dt;
        if !self.submitted && self.played >= self.threshold {
            self.submitted = true;
            return true;
        }
        false
    }
}

/// Vrai si passer au suivant à `position` secondes d'un morceau de `duration`
/// secondes est un « passé » : avant 50 % de la durée, ou avant 30 s si c'est
/// plus court. Durée inconnue : 30 s.
pub fn is_skip(position: f64, duration: f64) -> bool {
    let limit = if duration > 0.0 {
        (duration / 2.0).min(SKIP_MAX_SECS)
    } else {
        SKIP_MAX_SECS
    };
    position < limit
}

/// Mélange de Fisher-Yates ; `below(n)` tire un entier dans `0..n`.
pub fn shuffle<T>(items: &mut [T], mut below: impl FnMut(usize) -> usize) {
    for i in (1..items.len()).rev() {
        items.swap(i, below(i + 1));
    }
}

/// Tirage dans `0..n` sans biais notable (64 bits aléatoires du système).
pub fn random_below(n: usize) -> usize {
    let mut buf = [0u8; 8];
    if getrandom::fill(&mut buf).is_err() {
        return 0;
    }
    (u64::from_le_bytes(buf) % n.max(1) as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seuil_d_ecoute() {
        assert_eq!(scrobble_threshold(200.0), 100.0);
        assert_eq!(scrobble_threshold(600.0), 240.0);
        assert_eq!(scrobble_threshold(480.0), 240.0);
        assert_eq!(scrobble_threshold(0.0), 240.0);
        assert_eq!(scrobble_threshold(3.0), 1.5);
    }

    #[test]
    fn ecoute_a_cinquante_pour_cent_une_seule_fois() {
        let mut c = ScrobbleClock::new(200.0);
        assert!(!c.advance(99.0, true));
        assert!(c.advance(1.0, true));
        assert!(!c.advance(50.0, true), "signalée une seule fois");
    }

    #[test]
    fn ecoute_a_quatre_minutes_pour_un_long_morceau() {
        let mut c = ScrobbleClock::new(1200.0);
        assert!(!c.advance(239.0, true));
        assert!(c.advance(1.0, true));
    }

    #[test]
    fn les_pauses_ne_comptent_pas() {
        let mut c = ScrobbleClock::new(100.0);
        assert!(!c.advance(40.0, true));
        for _ in 0..100 {
            assert!(!c.advance(10.0, false));
        }
        assert_eq!(c.played(), 40.0);
        assert!(!c.advance(9.0, true));
        assert!(c.advance(1.0, true));
    }

    #[test]
    fn morceaux_passes() {
        // 200 s : limite 30 s (plus court que 100 s).
        assert!(is_skip(10.0, 200.0));
        assert!(!is_skip(30.0, 200.0));
        assert!(!is_skip(120.0, 200.0));
        // 40 s : limite 20 s (50 % plus court que 30 s).
        assert!(is_skip(19.0, 40.0));
        assert!(!is_skip(20.0, 40.0));
        // 3 s : limite 1,5 s.
        assert!(is_skip(1.0, 3.0));
        assert!(!is_skip(2.0, 3.0));
        // durée inconnue
        assert!(is_skip(29.0, 0.0));
        assert!(!is_skip(31.0, 0.0));
    }

    #[test]
    fn melange_est_une_permutation() {
        let mut v: Vec<u32> = (0..50).collect();
        let mut n = 7usize;
        shuffle(&mut v, |m| {
            n = (n * 31 + 11) % 1009;
            n % m
        });
        let mut sorted = v.clone();
        sorted.sort();
        assert_eq!(sorted, (0..50).collect::<Vec<_>>());
        assert_ne!(v, sorted);
        let mut one = vec![1];
        shuffle(&mut one, |_| 0);
        assert_eq!(one, [1]);
        assert!(random_below(5) < 5);
    }
}

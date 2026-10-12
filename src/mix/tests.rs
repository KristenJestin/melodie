use super::*;
use crate::db;

const NOW: i64 = 1_790_000_000;

fn new_lib() -> Library {
    Library {
        tracks: Vec::new(),
        artists: HashMap::new(),
        albums: HashMap::new(),
        genre_names: Vec::new(),
        genre_index: HashMap::new(),
        genre_sets: vec![Vec::new()],
        skipped: HashSet::new(),
        hot_plays: u32::MAX,
        now: NOW,
    }
}

/// Description d'un morceau factice.
#[derive(Clone)]
struct Spec {
    artist: u32,
    album: u32,
    genre: &'static str,
    plays: u32,
    rating: u8,
    starred: bool,
    played_ago: Option<i64>,
}

fn spec(artist: u32, album: u32) -> Spec {
    Spec {
        artist,
        album,
        genre: "Rock",
        plays: 0,
        rating: 0,
        starred: false,
        played_ago: None,
    }
}

fn build(specs: &[Spec]) -> Library {
    let mut lib = new_lib();
    let mut sets: HashMap<&str, u32> = HashMap::new();
    for (n, s) in specs.iter().enumerate() {
        let g = *sets.entry(s.genre).or_insert_with(|| {
            if s.genre.is_empty() {
                0
            } else {
                lib.add_genre_set(&format!("[\"{}\"]", s.genre))
            }
        });
        lib.tracks.push(Track {
            rowid: n as i64 + 1,
            artist: s.artist,
            album: s.album,
            genres: g,
            plays: s.plays,
            rating: s.rating,
            starred: s.starred,
            played: s.played_ago.map_or(0, |a| NOW - a),
        });
    }
    lib.hot_plays = hot_threshold(&lib.tracks);
    lib
}

fn all(lib: &Library) -> Vec<usize> {
    (0..lib.tracks.len()).collect()
}

fn no_dups(v: &[usize]) {
    let set: HashSet<_> = v.iter().collect();
    assert_eq!(set.len(), v.len(), "doublons dans {v:?}");
}

#[test]
fn dates_rfc3339() {
    assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(parse_rfc3339("2026-10-08T12:00:00Z"), Some(1_791_460_800));
    assert_eq!(
        parse_rfc3339("2026-10-08T12:00:00.123456789Z"),
        Some(1_791_460_800)
    );
    assert_eq!(
        parse_rfc3339("2026-10-08T14:00:00+02:00"),
        Some(1_791_460_800)
    );
    assert_eq!(
        parse_rfc3339("2026-10-08T10:30:00-01:30"),
        Some(1_791_460_800)
    );
    assert_eq!(parse_rfc3339("2026-10-08"), None);
    assert_eq!(parse_rfc3339("pas une date"), None);
    // Cohérent avec l'écriture de poste-core.
    assert_eq!(
        parse_rfc3339(&poste_core::time::rfc3339(1_234_567_890)),
        Some(1_234_567_890)
    );
    // 29 février d'une année bissextile.
    assert_eq!(parse_rfc3339("2024-02-29T00:00:00Z"), Some(1_709_164_800));
}

#[test]
fn exclusions_notes_et_morceaux_passes() {
    // 100 morceaux : 0..10 notés 1, 10..20 notés 2, 20..30 notés 3, 30..40 passés
    // récemment, 40..50 passés il y a plus de 7 jours, le reste neutre.
    let specs: Vec<Spec> = (0..100)
        .map(|i| {
            let mut s = spec(i, i);
            match i {
                0..10 => s.rating = 1,
                10..20 => s.rating = 2,
                20..30 => s.rating = 3,
                _ => {}
            }
            s
        })
        .collect();
    let mut lib = build(&specs);
    for i in 30..40 {
        lib.skipped.insert(lib.tracks[i].rowid);
    }
    // (la table skips ne contient que la fenêtre de 7 jours : voir le test de chargement)
    let mut rng = Rng::new(1);
    let mix = generate(&lib, &all(&lib), 100, None, None, &mut rng);
    no_dups(&mix);
    assert_eq!(mix.len(), 70, "20 notés 1-2 et 10 passés sont exclus");
    for i in &mix {
        assert!(!(0..20).contains(i) && !(30..40).contains(i), "{i}");
    }
    assert!(
        mix.iter().any(|i| (20..30).contains(i)),
        "3 étoiles accepté"
    );
}

#[test]
fn ecoutes_des_24h_exclues_si_assez_de_candidats() {
    let specs: Vec<Spec> = (0..100)
        .map(|i| {
            let mut s = spec(i, i);
            if i < 30 {
                s.plays = 2;
                s.played_ago = Some(3600);
            }
            s
        })
        .collect();
    let lib = build(&specs);
    let mut rng = Rng::new(2);
    // 70 frais pour 50 demandés : aucun des 30 récents.
    let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
    assert_eq!(mix.len(), 50);
    assert!(mix.iter().all(|i| *i >= 30));
    // 70 frais pour 90 demandés : pas assez, les récents reviennent.
    let mix = generate(&lib, &all(&lib), 90, None, None, &mut rng);
    assert_eq!(mix.len(), 90);
    assert!(mix.iter().any(|i| *i < 30));
    // Joué il y a plus de 24 h : pas exclu.
    let mut specs = specs;
    for s in &mut specs {
        s.played_ago = s.played_ago.map(|_| 2 * 86_400);
    }
    let lib = build(&specs);
    let mix = generate(&lib, &all(&lib), 100, None, None, &mut rng);
    assert_eq!(mix.len(), 100);
}

#[test]
fn paniers() {
    let mut specs = vec![
        Spec {
            starred: true,
            ..spec(0, 0)
        },
        Spec {
            rating: 4,
            ..spec(1, 1)
        },
        Spec {
            plays: 1,
            ..spec(2, 2)
        },
        Spec {
            rating: 3,
            ..spec(3, 3)
        },
        spec(4, 4),
    ];
    // Beaucoup d'écoutes = parmi les 20 % les plus écoutés.
    for i in 0..10 {
        specs.push(Spec {
            plays: 1 + i,
            ..spec(10 + i, 10 + i)
        });
    }
    let lib = build(&specs);
    let b = |i: usize| lib.basket(&lib.tracks[i]);
    assert_eq!((b(0), b(1)), (0, 0), "aimé, noté 4");
    assert_eq!((b(2), b(3)), (1, 1), "écouté une fois, noté 3");
    assert_eq!(b(4), 2, "découverte");
    // 11 morceaux écoutés (1 + 1..=10) : 20 % arrondis au-dessus = 3, seuil = 8.
    assert_eq!(lib.hot_plays, 8);
    assert_eq!(b(12), 0, "plays 8");
    assert_eq!(b(11), 1, "plays 7");
}

fn basket_library(a: usize, b: usize, c: usize) -> Library {
    let mut specs = Vec::new();
    let mut n = 0;
    // Des morceaux détestés mais très écoutés (donc exclus) fixent le seuil des
    // « 20 % les plus écoutés » au-dessus des familiers.
    for _ in 0..(a + b + c) / 2 {
        specs.push(Spec {
            rating: 1,
            plays: 50,
            ..spec(n, n)
        });
        n += 1;
    }
    for _ in 0..a {
        specs.push(Spec {
            starred: true,
            ..spec(n, n)
        });
        n += 1;
    }
    for _ in 0..b {
        specs.push(Spec {
            plays: 1,
            ..spec(n, n)
        });
        n += 1;
    }
    for _ in 0..c {
        specs.push(spec(n, n));
        n += 1;
    }
    build(&specs)
}

fn basket_counts(lib: &Library, mix: &[usize]) -> [usize; 3] {
    let mut c = [0; 3];
    for i in mix {
        c[lib.basket(&lib.tracks[*i])] += 1;
    }
    c
}

#[test]
fn proportions_des_paniers() {
    // Bibliothèque équilibrée : 40/35/25.
    let lib = basket_library(300, 300, 300);
    let mut sum = [0usize; 3];
    for seed in 0..40 {
        let mut rng = Rng::new(seed);
        let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
        assert_eq!(mix.len(), 50);
        let c = basket_counts(&lib, &mix);
        for k in 0..3 {
            sum[k] += c[k];
        }
        assert!(
            (19..=21).contains(&c[0]) && (16..=19).contains(&c[1]) && (11..=14).contains(&c[2]),
            "{c:?}"
        );
    }
    let avg = |k: usize| sum[k] as f64 / 40.0;
    assert!(
        (avg(0) - 20.0).abs() < 1.0 && (avg(1) - 17.5).abs() < 1.0 && (avg(2) - 12.5).abs() < 1.0
    );

    // Beaucoup d'inconnu (|C| > 3 (|A|+|B|)) : 30/30/40.
    let lib = basket_library(50, 50, 600);
    let mut rng = Rng::new(7);
    let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
    let c = basket_counts(&lib, &mix);
    assert!(
        (14..=16).contains(&c[0]) && (14..=16).contains(&c[1]) && (19..=21).contains(&c[2]),
        "{c:?}"
    );
    assert_eq!(
        shares(10, 10, 60),
        [0.4, 0.35, 0.25],
        "60 = 3 x 20 : pas encore"
    );
    assert_eq!(shares(10, 10, 61), [0.3, 0.3, 0.4]);
}

#[test]
fn panier_manquant_redistribue() {
    // Pas d'aimés : 35/25 redistribués entre familiers et découverte.
    let lib = basket_library(0, 200, 200);
    let mut rng = Rng::new(3);
    let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
    let c = basket_counts(&lib, &mix);
    assert_eq!(c[0], 0);
    assert_eq!(c[1] + c[2], 50);
    assert!((c[1] as i64 - 29).abs() <= 2, "{c:?}");
    // Panier trop petit : 5 aimés seulement, le reste est repris ailleurs.
    let lib = basket_library(5, 200, 200);
    let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
    let c = basket_counts(&lib, &mix);
    assert_eq!(c[0], 5);
    assert_eq!(c.iter().sum::<usize>(), 50);
    // Un seul panier.
    let lib = basket_library(0, 0, 80);
    let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
    assert_eq!(basket_counts(&lib, &mix), [0, 0, 50]);
}

/// Écart minimal entre deux morceaux de même clé.
fn min_gap(mix: &[usize], key: impl Fn(usize) -> u32) -> usize {
    let mut last: HashMap<u32, usize> = HashMap::new();
    let mut gap = usize::MAX;
    for (p, &i) in mix.iter().enumerate() {
        if let Some(l) = last.insert(key(i), p) {
            gap = gap.min(p - l);
        }
    }
    gap
}

fn varied_library() -> Library {
    // 40 artistes x 3 albums x 10 morceaux, quelques écoutes.
    let mut specs = Vec::new();
    for ar in 0..40u32 {
        for al in 0..3u32 {
            for t in 0..10u32 {
                specs.push(Spec {
                    plays: if t == 0 { 3 } else { 0 },
                    starred: t == 1 && al == 0,
                    ..spec(ar, ar * 3 + al)
                });
            }
        }
    }
    build(&specs)
}

#[test]
fn etalement_des_artistes_et_albums() {
    let lib = varied_library();
    for seed in 0..300 {
        let mut rng = Rng::new(seed);
        let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
        assert_eq!(mix.len(), 50);
        no_dups(&mix);
        assert!(
            min_gap(&mix, |i| lib.tracks[i].artist) >= 4,
            "seed {seed}: {mix:?}"
        );
        assert!(min_gap(&mix, |i| lib.tracks[i].album) >= 6, "seed {seed}");
    }
}

#[test]
fn plafond_de_10_pour_cent_par_artiste() {
    // Un artiste énorme (500 morceaux) contre 30 petits : au plus 5 sur 50.
    let mut specs: Vec<Spec> = (0..500)
        .map(|i| Spec {
            starred: true,
            ..spec(0, i / 10)
        })
        .collect();
    for ar in 1..=30u32 {
        for t in 0..20u32 {
            specs.push(spec(ar, 1000 + ar * 2 + t / 10));
        }
    }
    let lib = build(&specs);
    for seed in 0..20 {
        let mut rng = Rng::new(seed);
        let mix = generate(&lib, &all(&lib), 50, None, None, &mut rng);
        let big = mix.iter().filter(|i| lib.tracks[**i].artist == 0).count();
        assert!(big <= 5, "seed {seed}: {big}");
        assert_eq!(mix.len(), 50);
    }
    // Pas assez d'alternatives : le plafond cède plutôt que de raccourcir le mix.
    let mut specs: Vec<Spec> = (0..40).map(|i| spec(0, i / 10)).collect();
    specs.extend((0..5).map(|i| spec(1, 100 + i)));
    let lib = build(&specs);
    let mut rng = Rng::new(1);
    let mix = generate(&lib, &all(&lib), 30, None, None, &mut rng);
    assert_eq!(mix.len(), 30);
    let small = mix.iter().filter(|i| lib.tracks[**i].artist == 1).count();
    assert!(
        (3..=5).contains(&small),
        "le plafond cède pour compléter : {small}"
    );
    // Un seul artiste : pas de blocage.
    let lib = build(&(0..20).map(|i| spec(0, i)).collect::<Vec<_>>());
    assert_eq!(
        generate(&lib, &all(&lib), 10, None, None, &mut rng).len(),
        10
    );
    assert!(generate(&build(&[]), &[], 10, None, None, &mut rng).is_empty());
}

#[test]
fn arrange_respecte_les_ecarts_quand_c_est_faisable() {
    // 3 artistes x 4 morceaux, albums tous distincts : faisable avec un écart de 4 ? Non (3 artistes).
    // 5 artistes x 4 morceaux = 20 : faisable.
    let specs: Vec<Spec> = (0..20).map(|i| spec(i % 5, i)).collect();
    let lib = build(&specs);
    for seed in 0..500 {
        let mut rng = Rng::new(seed);
        let mut rest = all(&lib);
        rng.shuffle(&mut rest);
        let out = arrange(&lib, None, rest);
        assert_eq!(out.len(), 20);
        assert!(
            min_gap(&out, |i| lib.tracks[i].artist) >= 4,
            "seed {seed}: {out:?}"
        );
    }
    // Infaisable (2 artistes) : tout est quand même placé, sans doublon.
    let specs: Vec<Spec> = (0..10).map(|i| spec(i % 2, i)).collect();
    let lib = build(&specs);
    let out = arrange(&lib, Some(0), (1..10).collect());
    assert_eq!(out.len(), 10);
    no_dups(&out);
    assert_eq!(out[0], 0);
}

#[test]
fn determinisme_avec_la_graine() {
    let lib = varied_library();
    let run = |seed| generate(&lib, &all(&lib), 50, None, None, &mut Rng::new(seed));
    assert_eq!(run(1), run(1));
    assert_ne!(run(1), run(2));
}

#[test]
fn artiste_source_poids_et_plafond() {
    // Source : artiste 0 (150 morceaux sur 1000, même genre que les autres).
    let mut specs: Vec<Spec> = (0..150).map(|i| spec(0, i / 12)).collect();
    for ar in 1..=85u32 {
        for t in 0..10u32 {
            specs.push(spec(ar, 1000 + ar * 2 + t / 5));
        }
    }
    let lib = build(&specs);
    let src: Vec<usize> = (0..150).collect();
    let mut total = 0;
    for seed in 0..20 {
        let mut rng = Rng::new(seed);
        let mix = instant(&lib, &Source::Artist(src.clone()), 50, &mut rng);
        assert_eq!(mix.len(), 50);
        let n = mix.iter().filter(|i| lib.tracks[**i].artist == 0).count();
        assert!(n <= 15, "plafond de 30 % : {n}");
        total += n;
        let others = (1..=85)
            .map(|a| mix.iter().filter(|i| lib.tracks[**i].artist == a).count())
            .max()
            .unwrap();
        assert!(others <= 5, "10 % pour les autres : {others}");
        assert!(lib.tracks[mix[0]].artist == 0, "la source ouvre le mix");
    }
    // Poids x3 : 450 contre 850, donc largement au plafond.
    assert!(total as f64 / 20.0 > 12.0, "{}", total as f64 / 20.0);
    // Sans le poids, un mix d'album de la même artiste en prend bien moins.
    let mut rng = Rng::new(1);
    let album: Vec<usize> = (0..12).collect();
    let mix = instant(&lib, &Source::Album(album), 50, &mut rng);
    assert!(mix.iter().filter(|i| lib.tracks[**i].artist == 0).count() <= 5);
}

#[test]
fn ouverture_du_mix_instantane() {
    let mut specs: Vec<Spec> = (0..10)
        .map(|i| Spec {
            plays: i,
            ..spec(0, 0)
        })
        .collect();
    specs[3].starred = true;
    specs.extend((1..40).map(|i| spec(i, i)));
    let lib = build(&specs);
    let mut rng = Rng::new(5);
    // Album : l'aimé ouvre.
    let mix = instant(&lib, &Source::Album((0..10).collect()), 30, &mut rng);
    assert_eq!(mix[0], 3);
    // Morceau : lui-même, même s'il est noté une étoile.
    let mut specs2 = specs.clone();
    specs2[7].rating = 1;
    let lib2 = build(&specs2);
    let mix = instant(&lib2, &Source::Track(7), 30, &mut rng);
    assert_eq!(mix[0], 7);
    assert_eq!(mix.iter().filter(|i| **i == 7).count(), 1);
    // Sans titre aimé : le plus écouté (9 lectures).
    specs[3].starred = false;
    let lib3 = build(&specs);
    let mix = instant(&lib3, &Source::Artist((0..10).collect()), 30, &mut rng);
    assert_eq!(mix[0], 9);
}

#[test]
fn genres_et_complement_quand_peu_de_candidats() {
    // 6 morceaux Jazz (artistes 0 et 1), 300 Rock variés.
    let mut specs: Vec<Spec> = (0..4)
        .map(|i| Spec {
            genre: "Jazz",
            ..spec(0, i / 2)
        })
        .collect();
    specs.extend((0..2).map(|i| Spec {
        genre: "Jazz",
        ..spec(1, 10 + i)
    }));
    // 15 autres morceaux de l'artiste 0 dans un autre genre.
    specs.extend((0..15).map(|i| Spec {
        genre: "Blues",
        ..spec(0, 20 + i / 5)
    }));
    for ar in 2..60u32 {
        for t in 0..6u32 {
            specs.push(spec(ar, 100 + ar * 2 + t / 3));
        }
    }
    let lib = build(&specs);
    let jazz = lib.genre_index["jazz"];
    let mut rng = Rng::new(4);
    let mix = instant(&lib, &Source::Genre(jazz), 50, &mut rng);
    assert_eq!(mix.len(), 50, "complété jusqu'à la taille demandée");
    no_dups(&mix);
    // Les 6 jazz sont tous là (candidats rares, tirés en premier par les paniers).
    for j in 0..6 {
        assert!(mix.contains(&j), "jazz {j} absent");
    }
    // Le complément passe d'abord par l'artiste de la source (genre Blues), puis le reste.
    let mix = instant(&lib, &Source::Track(0), 50, &mut rng);
    assert_eq!(mix.len(), 50);
    assert_eq!(mix[0], 0);
    let blues = mix.iter().filter(|i| (6..21).contains(*i)).count();
    assert!(blues >= 4, "morceaux du même artiste ajoutés : {blues}");

    // Assez de candidats par les genres (>= 20) : pas de complément.
    let mut specs: Vec<Spec> = (0..25)
        .map(|i| Spec {
            genre: "Folk",
            ..spec(i, i)
        })
        .collect();
    specs.extend((25..200).map(|i| spec(i, i)));
    let lib = build(&specs);
    let mix = instant(&lib, &Source::Genre(lib.genre_index["folk"]), 50, &mut rng);
    assert_eq!(mix.len(), 25);
    assert!(mix.iter().all(|i| *i < 25));
    // Genre à plusieurs genres : l'union des genres des morceaux de la source.
    let mut lib = new_lib();
    let two = lib.add_genre_set("[\"Pop\",\"Dance\"]");
    let pop = lib.add_genre_set("[\"Pop\"]");
    let dance = lib.add_genre_set("[\"Dance\"]");
    let other = lib.add_genre_set("[\"Metal\"]");
    for (n, g) in [two, pop, dance, other]
        .iter()
        .cycle()
        .take(100)
        .enumerate()
    {
        lib.tracks.push(Track {
            rowid: n as i64 + 1,
            artist: n as u32,
            album: n as u32,
            genres: *g,
            plays: 0,
            rating: 0,
            starred: false,
            played: 0,
        });
    }
    let mix = instant(&lib, &Source::Track(0), 50, &mut rng);
    assert!(
        mix.iter()
            .filter(|i| lib.tracks[**i].genres != other)
            .count()
            >= 40
    );
}

#[test]
fn redecouvertes() {
    // 40 albums de 6 morceaux. 0..20 écoutés il y a 200 j, 20..30 écoutés hier, 30..40 jamais.
    let mut specs = Vec::new();
    for al in 0..40u32 {
        for _ in 0..6u32 {
            specs.push(Spec {
                plays: if al < 30 { 2 } else { 0 },
                ..spec(al / 2, al)
            });
        }
    }
    let lib = build(&specs);
    let stats: Vec<AlbumStat> = (0..40u32)
        .filter(|al| *al < 30)
        .map(|al| AlbumStat {
            key: lib.tracks[(al * 6) as usize].album,
            plays: 2,
        })
        .filter(|a| a.key < 20)
        .collect();
    assert_eq!(stats.len(), 20);
    for seed in 0..20 {
        let mut rng = Rng::new(seed);
        let mix = rediscover(&lib, &stats, 25, &mut rng);
        assert_eq!(mix.len(), 25);
        no_dups(&mix);
        let mut per_album: HashMap<u32, usize> = HashMap::new();
        for i in &mix {
            assert!(lib.tracks[*i].album < 20, "seul un album ancien est repris");
            *per_album.entry(lib.tracks[*i].album).or_insert(0) += 1;
        }
        assert!(
            per_album.values().all(|n| (1..=3).contains(n)),
            "{per_album:?}"
        );
        assert!(min_gap(&mix, |i| lib.tracks[i].album) >= 6 || per_album.len() < 12);
    }
    assert_eq!(
        rediscover(&lib, &stats, 50, &mut Rng::new(1)),
        rediscover(&lib, &stats, 50, &mut Rng::new(1))
    );
    assert!(rediscover(&lib, &[], 50, &mut Rng::new(1)).is_empty());
}

#[test]
fn genres_principaux() {
    let mut specs = Vec::new();
    for (g, n_hot, n_cold) in [
        ("Rock", 10, 5),
        ("Pop", 7, 0),
        ("Jazz", 3, 20),
        ("Folk", 1, 1),
        ("Metal", 2, 0),
        ("Soul", 4, 0),
        ("Rap", 5, 0),
        ("Dub", 6, 0),
    ] {
        for _ in 0..n_hot {
            specs.push(Spec {
                genre: g,
                starred: true,
                ..spec(specs.len() as u32, specs.len() as u32)
            });
        }
        for _ in 0..n_cold {
            specs.push(Spec {
                genre: g,
                ..spec(specs.len() as u32, specs.len() as u32)
            });
        }
    }
    let lib = build(&specs);
    let top = top_genres(&lib, 6);
    let names: Vec<_> = top.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, ["Rock", "Pop", "Dub", "Rap", "Soul", "Jazz"]);
    assert_eq!(top[0].1, 10);
    assert_eq!(top[0].2, 15);
    // Les morceaux détestés ne comptent pas.
    let mut specs2 = specs.clone();
    for s in specs2.iter_mut().filter(|s| s.genre == "Rock") {
        s.rating = 1;
    }
    let lib2 = build(&specs2);
    assert!(top_genres(&lib2, 6).iter().all(|t| t.0 != "Rock"));
}

// ---- base SQLite ----

fn insert_fake(conn: &Connection, n: usize) {
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut ar = conn
            .prepare("INSERT INTO artists(id, name) VALUES (?1, ?2)")
            .unwrap();
        let mut al = conn
            .prepare("INSERT INTO albums(id, name, artist, artistId, playCount, played) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")
            .unwrap();
        let mut tr = conn
            .prepare(
                "INSERT INTO tracks(id, title, artist, artistId, album, albumId, genres, genre, duration, suffix,
                   bitRate, playCount, played, starred, userRating) VALUES
                   (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 200, 'flac', 900, ?9, ?10, ?11, ?12)",
            )
            .unwrap();
        let genres = [
            "Rock",
            "Pop",
            "Jazz",
            "Electro",
            "Metal",
            "Folk",
            "Rap",
            "Classique",
            "Blues",
            "Soul",
        ];
        let n_artists = n / 20;
        let n_albums = n / 10;
        for a in 0..n_artists {
            ar.execute(params![format!("ar{a}"), format!("Artiste {a}")])
                .unwrap();
        }
        for a in 0..n_albums {
            let played: Option<String> = (a % 3 == 0)
                .then(|| poste_core::time::rfc3339((NOW - (a as i64 % 400) * 86_400) as u64));
            al.execute(params![
                format!("al{a}"),
                format!("Album {a}"),
                format!("Artiste {}", a / 2),
                format!("ar{}", a / 2),
                (a % 5) as i64,
                played
            ])
            .unwrap();
        }
        for i in 0..n {
            let a = (i / 10) / 2;
            let g1 = genres[i % genres.len()];
            let g2 = genres[(i / 7) % genres.len()];
            let raw = format!("[\"{g1}\",\"{g2}\"]");
            let played: Option<String> = (i % 4 == 0)
                .then(|| poste_core::time::rfc3339((NOW - (i as i64 % 500) * 3600 * 5) as u64));
            let starred: Option<String> = (i % 17 == 0).then(|| "2026-01-01T00:00:00Z".to_string());
            tr.execute(params![
                format!("t{i}"),
                format!("Titre {i}"),
                format!("Artiste {a}"),
                format!("ar{a}"),
                format!("Album {}", i / 10),
                format!("al{}", i / 10),
                raw,
                g1,
                (i % 9) as i64,
                played,
                starred,
                (i % 23 == 0) as i64 * ((i % 5) as i64)
            ])
            .unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
}

use rusqlite::params;

#[test]
fn chargement_depuis_la_copie() {
    let d = tempfile::tempdir().unwrap();
    let conn = db::create(&d.path().join("l.db")).unwrap();
    conn.execute_batch(
        "INSERT INTO tracks(id, title, artist, artistId, album, albumId, genres, genre, playCount, played, starred, userRating)
         VALUES ('a', 'A', 'X', 'ar1', 'Al', 'al1', '[\"Rock\",\"rock\",\"Pop\"]', 'Rock', 3, '2026-10-02T00:00:00.5Z', '2026-01-01T00:00:00Z', 4),
                ('b', 'B', 'X', 'ar1', 'Al', 'al1', NULL, 'Folk', 0, NULL, NULL, 3),
                ('c', 'C', 'Y', NULL, 'Sans id', NULL, NULL, NULL, 1, 'pas une date', NULL, 0),
                ('d', 'D', 'Y', NULL, 'Sans id', NULL, '[]', NULL, 1, NULL, NULL, 0);
         INSERT INTO skips(id, at) VALUES ('a', 1790000000 - 3600), ('b', 1790000000 - 8 * 86400);",
    )
    .unwrap();
    let lib = Library::load(&conn, NOW).unwrap();
    assert_eq!(lib.tracks.len(), 4);
    assert_eq!(lib.tracks[0].plays, 3);
    assert_eq!(lib.tracks[0].rating, 4);
    assert!(lib.tracks[0].starred && !lib.tracks[1].starred);
    assert_eq!(
        lib.tracks[0].played,
        parse_rfc3339("2026-10-02T00:00:00Z").unwrap()
    );
    assert_eq!(lib.tracks[2].played, 0, "date illisible = jamais");
    // Genres : doublons de casse fusionnés, ensemble vide pour [] ou rien.
    let set = |i: usize| lib.genre_sets[lib.tracks[i].genres as usize].len();
    assert_eq!((set(0), set(1), set(2), set(3)), (2, 1, 0, 0));
    assert_eq!(lib.genre_names, ["Rock", "Pop", "Folk"]);
    // Artistes et albums : l'identifiant manquant se rabat sur le nom.
    assert_eq!(lib.tracks[0].artist, lib.tracks[1].artist);
    assert_eq!(lib.tracks[2].album, lib.tracks[3].album);
    assert_ne!(lib.tracks[0].album, lib.tracks[2].album);
    // Passés : seulement ceux des 7 derniers jours.
    assert!(lib.skipped.contains(&1) && !lib.skipped.contains(&2));
    assert!(lib.excluded(&lib.tracks[0]), "passé récemment");
    assert!(
        !lib.excluded(&lib.tracks[1]),
        "passé il y a 8 jours : de nouveau permis"
    );
}

#[test]
fn copie_sans_table_des_passes() {
    let d = tempfile::tempdir().unwrap();
    let conn = db::create(&d.path().join("l.db")).unwrap();
    conn.execute_batch("DROP TABLE skips").unwrap();
    insert_fake(&conn, 100);
    let lib = Library::load(&conn, NOW).unwrap();
    assert_eq!(lib.tracks.len(), 100);
    assert!(lib.skipped.is_empty());
}

#[test]
fn sources_par_identifiant_ou_nom() {
    let d = tempfile::tempdir().unwrap();
    let conn = db::create(&d.path().join("l.db")).unwrap();
    insert_fake(&conn, 200);
    let lib = Library::load(&conn, NOW).unwrap();
    let src = |k, s: &str| find_source(&conn, &lib, k, s);
    assert_eq!(src(SourceKind::Track, "t5"), Ok(Source::Track(5)));
    assert_eq!(src(SourceKind::Track, "titre 5"), Ok(Source::Track(5)));
    assert!(src(SourceKind::Track, "zzz").is_err());
    let Ok(Source::Album(v)) = src(SourceKind::Album, "al3") else {
        panic!()
    };
    assert_eq!(v, (30..40).collect::<Vec<_>>());
    assert_eq!(
        src(SourceKind::Album, "ALBUM 3"),
        src(SourceKind::Album, "al3")
    );
    let Ok(Source::Artist(v)) = src(SourceKind::Artist, "artiste 1") else {
        panic!()
    };
    assert_eq!(v, (20..40).collect::<Vec<_>>());
    assert!(matches!(
        src(SourceKind::Genre, "JAZZ"),
        Ok(Source::Genre(_))
    ));
    assert!(src(SourceKind::Genre, "Disco")
        .unwrap_err()
        .contains("genre inconnu"));
    // Détails dans l'ordre du mix.
    let det = details(&conn, &lib, &[7, 3]).unwrap();
    assert_eq!(det[0].info.id, "t7");
    assert_eq!(det[1].info.album, "Album 0");
    assert_eq!(det[1].info.album_artist, "Artiste 0");
    assert_eq!(det[0].info.quality(), "FLAC · 900 kbps");
    assert!(!det[0].info.starred);
    let det = details(&conn, &lib, &[0]).unwrap();
    assert!(det[0].info.starred);
}

#[test]
fn temps_sur_40000_morceaux() {
    let d = tempfile::tempdir().unwrap();
    let conn = db::create(&d.path().join("l.db")).unwrap();
    insert_fake(&conn, 40_000);
    // Plusieurs mesures : la première chauffe le cache de pages.
    let limit_ms: u128 = if cfg!(debug_assertions) { 2000 } else { 150 };
    let mut worst = std::time::Duration::ZERO;
    for round in 0..4 {
        let t = std::time::Instant::now();
        let lib = Library::load(&conn, NOW).unwrap();
        let mut rng = Rng::new(round);
        let mix = personal(&lib, DEFAULT_SIZE, &mut rng);
        let _ = details(&conn, &lib, &mix).unwrap();
        let elapsed = t.elapsed();
        assert_eq!(mix.len(), 50);
        eprintln!("mix perso sur 40000 morceaux (chargement compris) : {elapsed:?}");
        worst = worst.max(elapsed);
    }
    let t = std::time::Instant::now();
    let lib = Library::load(&conn, NOW).unwrap();
    let genre = lib.genre_index["jazz"];
    let mix = instant(&lib, &Source::Genre(genre), 50, &mut Rng::new(1));
    let _ = details(&conn, &lib, &mix).unwrap();
    let instant_t = t.elapsed();
    eprintln!("mix genre : {instant_t:?}");
    let t = std::time::Instant::now();
    let lib = Library::load(&conn, NOW).unwrap();
    let stale = stale_albums(&conn, &lib).unwrap();
    let mix = rediscover(&lib, &stale, 50, &mut Rng::new(1));
    let _ = details(&conn, &lib, &mix).unwrap();
    let reco_t = t.elapsed();
    eprintln!("redécouvertes ({} albums) : {reco_t:?}", stale.len());
    assert!(!mix.is_empty());
    let t = std::time::Instant::now();
    let lib = Library::load(&conn, NOW).unwrap();
    let top = top_genres(&lib, 6);
    eprintln!("genres principaux : {:?} ({top:?})", t.elapsed());
    assert_eq!(top.len(), 6);
    assert!(worst.as_millis() < limit_ms, "trop lent : {worst:?}");
    assert!(instant_t.as_millis() < limit_ms && reco_t.as_millis() < limit_ms);
}

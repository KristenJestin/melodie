//! Editing playlists from the window, without the window: the question asked when what is added
//! is already in the playlist, and the duplicates shown before they are removed. Both are read
//! from the `--json` outputs of `melodie playlist add --dry-run` and `playlist duplicates`.

use crate::launch::{Adding, Job, read_json};
use serde_json::Value;

/// What `playlist add --dry-run` said: the playlist's name, what would go in and what is there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub name: String,
    /// Titles of the tracks that would be added.
    pub added: Vec<String>,
    /// Titles of the tracks already there.
    pub already: Vec<String>,
}

/// The duplicate question: what it says, and the two answers with what each runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub title: String,
    /// The titles of the tracks already there, to show.
    pub already: Vec<String>,
    /// Leave them out; `None` when nothing is left to add.
    pub skip: (String, Option<Job>),
    /// Add them all the same.
    pub anyway: (String, Job),
}

/// What comes after the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Nothing is there yet: add without asking.
    Run(Job),
    Ask(Question),
}

fn titles(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    t["title"]
                        .as_str()
                        .or(t["id"].as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn name_of(v: &Value) -> String {
    v["playlist"]["name"]
        .as_str()
        .unwrap_or("the playlist")
        .to_string()
}

fn tracks(n: usize) -> String {
    if n == 1 {
        "1 track".into()
    } else {
        format!("{n} tracks")
    }
}

impl Checked {
    pub fn read(out: &str) -> Result<Self, String> {
        let v = read_json(out, "Could not read the playlist")?;
        Ok(Self {
            name: name_of(&v),
            added: titles(&v, "added"),
            already: titles(&v, "already"),
        })
    }

    /// Adds without asking when nothing is there yet; otherwise the question.
    pub fn next(&self, playlist: &str, what: &Adding) -> Next {
        let add = |allow_duplicates| Job::AddToPlaylist {
            playlist: playlist.to_string(),
            what: what.clone(),
            allow_duplicates,
        };
        let (new, old) = (self.added.len(), self.already.len());
        if old == 0 {
            return Next::Run(add(false));
        }
        let name = &self.name;
        let (title, skip, anyway) = if new == 0 {
            let title = match &self.already[..] {
                [one] => format!("{one} is already in {name}"),
                _ => format!("All {old} tracks are already in {name}"),
            };
            (title, ("Skip".to_string(), None), "Add anyway".to_string())
        } else {
            let others = if new == 1 {
                "Add the other one".to_string()
            } else {
                format!("Add the {new} others")
            };
            (
                format!("{old} of {} are already in {name}", tracks(new + old)),
                (others, Some(add(false))),
                format!("Add all {}", new + old),
            )
        };
        Next::Ask(Question {
            title,
            already: self.already.clone(),
            skip,
            anyway: (anyway, add(true)),
        })
    }
}

/// A track met again in a playlist, by rank counted from 1 as people read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repeat {
    pub rank: usize,
    /// The track there, to check it still is when it is removed.
    pub id: String,
    /// The rank of the occurrence it repeats.
    pub first: usize,
    pub title: String,
    pub artist: String,
}

/// What `playlist duplicates --json` found: `exact` (same track id) go when removed, `similar`
/// (another id, same title and artist) are only shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    pub total: usize,
    pub exact: Vec<Repeat>,
    pub similar: Vec<Repeat>,
}

fn repeats(v: &Value, key: &str) -> Vec<Repeat> {
    let n = |d: &Value, k: &str| d[k].as_u64().unwrap_or(0) as usize + 1;
    v[key]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|d| Repeat {
                    rank: n(d, "index"),
                    first: n(d, "first"),
                    id: d["track"]["id"].as_str().unwrap_or("").to_string(),
                    title: d["track"]["title"].as_str().unwrap_or("").to_string(),
                    artist: d["track"]["artist"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Found {
    pub fn read(out: &str) -> Result<Self, String> {
        let v = read_json(out, "Could not read the duplicates")?;
        Ok(Self {
            name: name_of(&v),
            total: v["total"].as_u64().unwrap_or(0) as usize,
            exact: repeats(&v, "exact"),
            similar: repeats(&v, "similar"),
        })
    }

    /// What removes the exact duplicates shown, and what it says when done; none when there are
    /// none.
    pub fn remove(&self, playlist: &str) -> Option<(Job, String)> {
        if self.exact.is_empty() {
            return None;
        }
        let job = Job::RemoveFromPlaylist {
            id: playlist.to_string(),
            picks: self
                .exact
                .iter()
                .map(|r| (r.rank - 1, r.id.clone()))
                .collect(),
        };
        let name = &self.name;
        let gone = match self.exact.len() {
            1 => format!("Removed 1 duplicate from {name}"),
            k => format!("Removed {k} duplicates from {name}"),
        };
        let said = match self.similar.len() {
            0 => gone,
            1 => format!("{gone} (1 likely duplicate left)"),
            k => format!("{gone} ({k} likely duplicates left)"),
        };
        Some((job, said))
    }

    /// The button that removes the exact duplicates; none when there are none.
    pub fn remove_label(&self) -> Option<String> {
        match self.exact.len() {
            0 => None,
            1 => Some("Remove 1 duplicate".into()),
            n => Some(format!("Remove {n} duplicates")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(added: &[&str], already: &[&str]) -> String {
        let t = |id: &&str| json!({"id": id, "title": format!("Title {id}"), "artist": "A", "album": "B"});
        json!({"playlist": {"id": "p1", "name": "Road"},
               "added": added.iter().map(t).collect::<Vec<_>>(),
               "already": already.iter().map(t).collect::<Vec<_>>(),
               "allowDuplicates": false, "dryRun": true})
        .to_string()
    }

    fn album() -> Adding {
        Adding::Album("al1".into())
    }

    fn add(allow_duplicates: bool) -> Job {
        Job::AddToPlaylist {
            playlist: "p1".into(),
            what: album(),
            allow_duplicates,
        }
    }

    #[test]
    fn nothing_already_there_adds_without_asking() {
        let c = Checked::read(&check(&["t1", "t2"], &[])).unwrap();
        let next = c.next("p1", &album());
        assert_eq!(next, Next::Run(add(false)));
    }

    #[test]
    fn some_already_there_asks_to_skip_them_or_add_them_anyway() {
        let c = Checked::read(&check(&["t1", "t2", "t3"], &["t4", "t5"])).unwrap();
        let Next::Ask(q) = c.next("p1", &album()) else {
            panic!("a question")
        };
        assert_eq!(q.title, "2 of 5 tracks are already in Road");
        assert_eq!(q.skip.0, "Add the 3 others");
        assert_eq!(q.skip.1, Some(add(false)));
        assert_eq!(q.anyway.0, "Add all 5");
        assert_eq!(q.anyway.1, add(true));
        assert_eq!(q.already, ["Title t4", "Title t5"]);
    }

    #[test]
    fn all_already_there_asks_to_skip_or_add_anyway() {
        let c = Checked::read(&check(&[], &["t1", "t2"])).unwrap();
        let Next::Ask(q) = c.next("p1", &album()) else {
            panic!("a question")
        };
        assert_eq!(q.title, "All 2 tracks are already in Road");
        assert_eq!(q.skip.0, "Skip");
        assert_eq!(q.skip.1, None, "skipping them all writes nothing");
        assert_eq!(q.anyway.0, "Add anyway");
        assert_eq!(q.anyway.1, add(true));
    }

    #[test]
    fn one_track_already_there_is_named() {
        let c = Checked::read(&check(&[], &["t1"])).unwrap();
        let Next::Ask(q) = c.next("p1", &Adding::Tracks(vec!["t1".into()])) else {
            panic!("a question")
        };
        assert_eq!(q.title, "Title t1 is already in Road");
    }

    #[test]
    fn a_failed_check_says_why_without_the_launcher_text() {
        let fail = r#"[{"id":"unreachable","name":"Navidrome injoignable","subtitle":"https://music.example.org"}]"#;
        assert_eq!(Checked::read(fail).unwrap_err(), "Navidrome is unreachable");
        assert!(Checked::read("not json").is_err());
    }

    #[test]
    fn the_duplicates_are_shown_by_rank_from_one() {
        let out = json!({"playlist": {"id": "p1", "name": "Road"}, "total": 6,
            "exact": [{"index": 3, "first": 0, "track": {"id": "t1", "title": "One", "artist": "A", "album": "B"}}],
            "similar": [{"index": 5, "first": 1, "track": {"id": "t9", "title": "Two", "artist": "C", "album": "D"}}],
            "removed": []})
        .to_string();
        let d = Found::read(&out).unwrap();
        assert_eq!(d.name, "Road");
        assert_eq!(d.total, 6);
        assert_eq!(
            d.exact,
            [Repeat {
                rank: 4,
                id: "t1".into(),
                first: 1,
                title: "One".into(),
                artist: "A".into()
            }]
        );
        assert_eq!(d.similar[0].rank, 6);
        assert_eq!(d.remove_label(), Some("Remove 1 duplicate".to_string()));
        let none = json!({"playlist": {"id": "p1", "name": "Road"}, "total": 2,
            "exact": [], "similar": [], "removed": []})
        .to_string();
        assert_eq!(Found::read(&none).unwrap().remove_label(), None);
    }

    #[test]
    fn removing_the_duplicates_removes_exactly_the_ranks_shown() {
        let out = json!({"playlist": {"id": "p1", "name": "Road"}, "total": 6,
            "exact": [{"index": 3, "first": 0, "track": {"id": "t1", "title": "One", "artist": "A", "album": "B"}},
                      {"index": 4, "first": 1, "track": {"id": "-t2", "title": "Two", "artist": "A", "album": "B"}}],
            "similar": [{"index": 5, "first": 1, "track": {"id": "t9", "title": "Two", "artist": "C", "album": "D"}}],
            "removed": []})
        .to_string();
        let d = Found::read(&out).unwrap();
        let (job, said) = d.remove("p1").unwrap();
        assert_eq!(
            job,
            Job::RemoveFromPlaylist {
                id: "p1".into(),
                picks: vec![(3, "t1".into()), (4, "-t2".into())],
            },
            "the ranks shown, each with the track shown there, and nothing else"
        );
        assert_eq!(
            job.args(),
            [
                "playlist",
                "remove",
                "--expect=t1",
                "--expect=-t2",
                "--",
                "p1",
                "4",
                "5"
            ]
        );
        assert_eq!(
            said,
            "Removed 2 duplicates from Road (1 likely duplicate left)"
        );
        let none = json!({"playlist": {"id": "p1", "name": "Road"}, "total": 2,
            "exact": [], "similar": [], "removed": []})
        .to_string();
        assert_eq!(Found::read(&none).unwrap().remove("p1"), None);
    }
}

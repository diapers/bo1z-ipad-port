//! The maps this build can run, and how each one's files are found.
//!
//! Two layouts under `game/`:
//!  * app bundle: `game/store/<ab>/<sha256>` holds every unique file once (maps share a lot)
//!    and `game/maps/<slug>.json` lists that map's `[path, sha256, size]` triples;
//!  * development: `game/<slug>/` is the map's plain file tree (e.g. `--game D:/bo1z/maps`).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::fs::{MountSrc, VDir};

/// (slug, zone, display name) in the site's map-select order (landing.js ENTRIES).
pub const ALL: &[(&str, &str, &str)] = &[
    ("kino", "zombie_theater", "Kino der Toten"),
    ("five", "zombie_pentagon", "Five"),
    ("riese", "zombie_cod5_factory", "Der Riese"),
    ("nacht", "zombie_cod5_prototype", "Nacht der Untoten"),
    ("verruckt", "zombie_cod5_asylum", "Verruckt"),
    ("shinonuma", "zombie_cod5_sumpf", "Shi No Numa"),
    ("ascension", "zombie_cosmodrome", "Ascension"),
    ("cotd", "zombie_coast", "Call of the Dead"),
    ("shangrila", "zombie_temple", "Shangri-La"),
    ("moon", "zombie_moon", "Moon"),
];

#[derive(Clone)]
enum Src {
    Dir(PathBuf),
    Index(PathBuf),
}

#[derive(Clone)]
pub struct MapInfo {
    pub slug: &'static str,
    pub zone: &'static str,
    pub name: &'static str,
    src: Src,
}

/// Every map present under `game`, in menu order.
pub fn discover(game: &Path) -> Vec<MapInfo> {
    ALL.iter()
        .filter_map(|&(slug, zone, name)| {
            let index = game.join("maps").join(format!("{slug}.json"));
            let dir = game.join(slug);
            let src = if index.is_file() {
                Src::Index(index)
            } else if dir.join("zone").is_dir() {
                Src::Dir(dir)
            } else {
                return None;
            };
            Some(MapInfo { slug, zone, name, src })
        })
        .collect()
}

impl MapInfo {
    /// The read-only mount for this map, plus the bytes of its zone (.ff) files (the
    /// loading bar's denominator).
    pub fn mount(&self, game: &Path) -> Result<(MountSrc, u64)> {
        match &self.src {
            Src::Dir(dir) => Ok((MountSrc::Dir(dir.clone()), ff_bytes(&dir.join("zone")))),
            Src::Index(index) => {
                let text = std::fs::read_to_string(index).with_context(|| format!("reading {}", index.display()))?;
                let v: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", index.display()))?;
                let files = v["files"].as_array().ok_or_else(|| anyhow!("{}: no files", index.display()))?;
                let store = game.join("store");
                let mut ff = 0u64;
                let mut list = Vec::with_capacity(files.len());
                for f in files {
                    let (Some(path), Some(sha), size) = (f[0].as_str(), f[1].as_str(), f[2].as_u64().unwrap_or(0)) else {
                        continue;
                    };
                    if sha.len() < 2 {
                        continue;
                    }
                    let lower = path.to_ascii_lowercase();
                    if lower.starts_with("zone/") && lower.ends_with(".ff") {
                        ff += size;
                    }
                    list.push((path.to_string(), store.join(&sha[..2]).join(sha)));
                }
                Ok((MountSrc::Index(VDir::from_files(list)), ff))
            }
        }
    }
}

fn ff_bytes(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                ff_bytes(&p)
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff")) {
                e.metadata().map_or(0, |m| m.len())
            } else {
                0
            }
        })
        .sum()
}

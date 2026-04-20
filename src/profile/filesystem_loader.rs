use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::errors::PriestError;
use super::default_profile::built_in_default;
use super::loader::ProfileLoader;
use super::model::Profile;

#[derive(Debug, Clone, PartialEq)]
struct CacheKey {
    max_mtime: u64,
    file_count: usize,
}

struct CacheEntry {
    key: CacheKey,
    profile: Profile,
}

pub struct FilesystemProfileLoader {
    profiles_root: PathBuf,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl FilesystemProfileLoader {
    pub fn new(profiles_root: impl Into<PathBuf>) -> Self {
        Self { profiles_root: profiles_root.into(), cache: Mutex::new(HashMap::new()) }
    }
}

impl ProfileLoader for FilesystemProfileLoader {
    fn load(&self, name: &str) -> Result<Profile, PriestError> {
        let profile_dir = self.profiles_root.join(name);
        let profile_md = profile_dir.join("PROFILE.md");

        if profile_md.exists() {
            let key = compute_cache_key(&profile_dir);

            // Cache hit check
            {
                let cache = self.cache.lock().unwrap();
                if let Some(entry) = cache.get(name) {
                    if entry.key == key {
                        return Ok(entry.profile.clone());
                    }
                }
            }

            let profile = load_from_dir(name, &profile_dir)?;

            {
                let mut cache = self.cache.lock().unwrap();
                cache.insert(name.to_string(), CacheEntry { key, profile: profile.clone() });
            }

            return Ok(profile);
        }

        if name == "default" {
            return Ok(built_in_default());
        }

        Err(PriestError::ProfileNotFound { profile: name.to_string() })
    }
}

fn compute_cache_key(dir: &Path) -> CacheKey {
    let mut files: Vec<PathBuf> = vec![];

    for filename in &["PROFILE.md", "RULES.md", "CUSTOM.md", "profile.toml"] {
        let p = dir.join(filename);
        if p.exists() {
            files.push(p);
        }
    }

    let memories_dir = dir.join("memories");
    if memories_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&memories_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if ext == "md" || ext == "txt" {
                        files.push(path);
                    }
                }
            }
        }
    }

    let file_count = files.len();
    let max_mtime = files
        .iter()
        .filter_map(|p| p.metadata().ok()?.modified().ok())
        .filter_map(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .max()
        .unwrap_or(0);

    CacheKey { max_mtime, file_count }
}

fn load_from_dir(name: &str, dir: &Path) -> Result<Profile, PriestError> {
    let read = |p: &Path| -> Result<String, PriestError> {
        std::fs::read_to_string(p).map_err(|e| PriestError::ProfileInvalid {
            profile: name.to_string(),
            reason: e.to_string(),
        })
    };

    let identity = read(&dir.join("PROFILE.md"))?;

    let rules_path = dir.join("RULES.md");
    let rules = if rules_path.exists() { read(&rules_path)? } else { String::new() };

    let custom_path = dir.join("CUSTOM.md");
    let custom = if custom_path.exists() { read(&custom_path)? } else { String::new() };

    let mut memories = vec![];
    let memories_dir = dir.join("memories");
    if memories_dir.is_dir() {
        let mut mem_files: Vec<PathBuf> = std::fs::read_dir(&memories_dir)
            .map_err(|e| PriestError::ProfileInvalid { profile: name.to_string(), reason: e.to_string() })?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).map(|e| e == "md" || e == "txt").unwrap_or(false)
            })
            .collect();
        mem_files.sort();
        for f in &mem_files {
            memories.push(read(f)?);
        }
    }

    Ok(Profile::new(name, identity, rules, custom, memories, Default::default()))
}

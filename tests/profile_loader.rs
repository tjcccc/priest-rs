use std::fs;
use std::time::SystemTime;
use tempfile::TempDir;
use priest::profile::filesystem_loader::FilesystemProfileLoader;
use priest::profile::loader::ProfileLoader;

fn make_profile_dir(tmp: &TempDir, name: &str, identity: &str) -> std::path::PathBuf {
    let dir = tmp.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("PROFILE.md"), identity).unwrap();
    dir
}

#[test]
fn loads_profile_from_directory() {
    let tmp = TempDir::new().unwrap();
    make_profile_dir(&tmp, "bot", "I am a bot.");
    let loader = FilesystemProfileLoader::new(tmp.path());
    let profile = loader.load("bot").unwrap();
    assert_eq!(profile.name, "bot");
    assert_eq!(profile.identity, "I am a bot.");
}

#[test]
fn falls_back_to_default_when_name_is_default() {
    let tmp = TempDir::new().unwrap();
    let loader = FilesystemProfileLoader::new(tmp.path());
    let profile = loader.load("default").unwrap();
    assert_eq!(profile.name, "default");
    assert!(!profile.identity.is_empty());
}

#[test]
fn throws_when_profile_not_found_and_not_default() {
    let tmp = TempDir::new().unwrap();
    let loader = FilesystemProfileLoader::new(tmp.path());
    assert!(loader.load("missing").is_err());
}

#[test]
fn loads_rules_and_custom() {
    let tmp = TempDir::new().unwrap();
    let dir = make_profile_dir(&tmp, "bot", "ID.");
    fs::write(dir.join("RULES.md"), "Be concise.").unwrap();
    fs::write(dir.join("CUSTOM.md"), "Custom text.").unwrap();
    let loader = FilesystemProfileLoader::new(tmp.path());
    let profile = loader.load("bot").unwrap();
    assert_eq!(profile.rules, "Be concise.");
    assert_eq!(profile.custom, "Custom text.");
}

#[test]
fn cache_hit_serves_stale_content_when_mtime_unchanged() {
    let tmp = TempDir::new().unwrap();
    let dir = make_profile_dir(&tmp, "bot", "v1.");
    let profile_md = dir.join("PROFILE.md");

    // Pin mtime to a fixed past value
    let pinned = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    filetime::set_file_mtime(&profile_md, filetime::FileTime::from_system_time(pinned)).unwrap();

    let loader = FilesystemProfileLoader::new(tmp.path());
    let first = loader.load("bot").unwrap();
    assert_eq!(first.identity, "v1.");

    // Overwrite content but restore same pinned mtime
    fs::write(&profile_md, "v2.").unwrap();
    filetime::set_file_mtime(&profile_md, filetime::FileTime::from_system_time(pinned)).unwrap();

    let second = loader.load("bot").unwrap();
    assert_eq!(second.identity, "v1.", "cache should serve stale entry when mtime unchanged");
}

#[test]
fn cache_invalidation_reloads_after_file_modified() {
    let tmp = TempDir::new().unwrap();
    make_profile_dir(&tmp, "bot", "Bot v1.");
    let loader = FilesystemProfileLoader::new(tmp.path());
    let first = loader.load("bot").unwrap();
    assert_eq!(first.identity, "Bot v1.");

    let profile_md = tmp.path().join("bot").join("PROFILE.md");
    fs::write(&profile_md, "Bot v2.").unwrap();
    // Advance mtime
    let future = SystemTime::now() + std::time::Duration::from_secs(2);
    filetime::set_file_mtime(&profile_md, filetime::FileTime::from_system_time(future)).unwrap();

    let second = loader.load("bot").unwrap();
    assert_eq!(second.identity, "Bot v2.");
}

#[test]
fn cache_invalidation_reloads_when_file_added() {
    let tmp = TempDir::new().unwrap();
    let dir = make_profile_dir(&tmp, "bot", "Bot.");
    let loader = FilesystemProfileLoader::new(tmp.path());
    let first = loader.load("bot").unwrap();
    assert!(first.rules.is_empty());

    let rules_md = dir.join("RULES.md");
    fs::write(&rules_md, "Be concise.").unwrap();
    let future = SystemTime::now() + std::time::Duration::from_secs(2);
    filetime::set_file_mtime(&rules_md, filetime::FileTime::from_system_time(future)).unwrap();

    let second = loader.load("bot").unwrap();
    assert_eq!(second.rules, "Be concise.");
}

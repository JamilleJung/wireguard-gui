//! Backup restore parsing (`src/import_parse.rs`): arbitrary bytes as a ZIP
//! archive. The preflight and parse must never panic, must stay within the
//! documented memory bounds, and every accepted archive must satisfy the
//! invariants restore relies on before its first privileged write.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;
#[path = "common/heap.rs"]
mod heap;
#[allow(dead_code)]
#[path = "../../src/import_parse.rs"]
mod import_parse;
#[allow(dead_code)]
#[path = "../../src/secrets.rs"]
mod secrets;
#[allow(dead_code)]
#[path = "../../src/validation.rs"]
mod validation;

use std::collections::HashSet;
use std::io::Cursor;

use import_parse::{
    MAX_ARCHIVE_ENTRIES, MAX_BACKUP_CONFIGS, MAX_CONFIG_BYTES, MAX_TOTAL_CONFIG_BYTES,
};
use libfuzzer_sys::fuzz_target;

/// Peak heap one archive may need: the retained configs (string capacity can
/// reach twice the length), the entry being read, preflight and inflate
/// buffers, and ZIP metadata, which grows with the archive's size.
fn heap_budget(archive_len: usize) -> usize {
    let fixed = 2 * MAX_TOTAL_CONFIG_BYTES + 4 * MAX_CONFIG_BYTES + 4 * 1024 * 1024;
    fixed as usize + 64 * archive_len
}

fuzz_target!(|data: &[u8]| {
    let (preflight, peak) = heap::measure(|| {
        import_parse::preflight_zip_directory(&mut Cursor::new(data), data.len() as u64)
    });
    // The preflight reads at most one 64 KiB chunk plus overlap, and a 128 KiB tail.
    assert!(peak <= 256 * 1024, "preflight held {peak} bytes");

    let (count, peak) = heap::measure(|| import_parse::backup_entry_count(Cursor::new(data)));
    assert!(peak <= heap_budget(data.len()), "listing held {peak} bytes");
    assert!(count as u64 <= MAX_ARCHIVE_ENTRIES);

    let (result, peak) = heap::measure(|| import_parse::backup_configs(Cursor::new(data)));
    assert!(
        peak <= heap_budget(data.len()),
        "restore preflight held {peak} bytes"
    );

    if preflight.is_err() {
        assert_eq!(count, 0, "listing counted an archive the preflight refused");
        assert!(
            result.is_err(),
            "restore accepted an archive the preflight refused"
        );
    }
    let Ok(configs) = result else {
        return;
    };
    assert!(!configs.is_empty());
    assert!(configs.len() <= MAX_BACKUP_CONFIGS);
    assert!(
        count >= configs.len(),
        "listing undercounted an accepted archive"
    );
    let zip =
        import_parse::open_backup_archive(Cursor::new(data)).expect("reopen accepted archive");
    let entries: HashSet<String> = zip
        .file_names()
        .filter_map(Result::ok)
        .map(|name| name.into_owned())
        .collect();
    let mut names = HashSet::new();
    let mut total = 0u64;
    for (name, content) in &configs {
        assert!(validation::validate_tunnel_name(name).is_ok(), "{name:?}");
        assert_eq!(name.trim(), name);
        assert!(names.insert(name), "duplicate tunnel {name:?}");
        assert!(entries.contains(&format!("{name}.conf")));
        assert!(content.len() as u64 <= MAX_CONFIG_BYTES);
        total += content.len() as u64;
        assert!(config::validate_basic_wireguard_config(content).is_ok());
        assert!(
            !secrets::config_runs_scripts(content),
            "accepted script hooks"
        );
    }
    assert!(total <= MAX_TOTAL_CONFIG_BYTES);
});

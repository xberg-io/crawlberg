use std::path::{Path, PathBuf};

const SENSITIVE_MARKER: &str = "#[cfg_attr(alef, alef(sensitive))]";

fn crawlberg_alef_sources(repo_root: &Path) -> Vec<String> {
    let config_path = repo_root.join("alef.toml");
    let source = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", config_path.display()));
    let config: toml::Value =
        toml::from_str(&source).unwrap_or_else(|error| panic!("failed to parse {}: {error}", config_path.display()));
    let crates = config
        .get("crates")
        .and_then(toml::Value::as_array)
        .expect("alef.toml must declare crates");
    let crawlberg = crates
        .iter()
        .find(|entry| entry.get("name").and_then(toml::Value::as_str) == Some("crawlberg"))
        .expect("alef.toml must declare the crawlberg crate");

    crawlberg
        .get("sources")
        .and_then(toml::Value::as_array)
        .expect("the crawlberg Alef crate must declare sources")
        .iter()
        .map(|source| source.as_str().expect("each Alef source must be a string").to_owned())
        .collect()
}

fn reviewed_sensitive_declarations(repo_root: &Path) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    for source_path in crawlberg_alef_sources(repo_root) {
        let path = repo_root.join(&source_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let lines: Vec<&str> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if line.trim() != SENSITIVE_MARKER {
                continue;
            }
            let declaration = lines
                .get(index + 1)
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .expect("a sensitive marker must immediately precede its field declaration");
            declarations.push((source_path.clone(), declaration.to_owned()));
        }
    }
    declarations.sort_unstable();
    declarations
}

#[test]
fn binding_sensitive_metadata_matches_reviewed_marker_inventory() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut expected = vec![
        (
            "crates/crawlberg/src/interact/actions.rs".to_owned(),
            "script: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/interact/actions.rs".to_owned(),
            "text: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config.rs".to_owned(),
            "pub custom_headers: HashMap<String, String>,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/credentials.rs".to_owned(),
            "password: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/credentials.rs".to_owned(),
            "pub password: Option<String>,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/credentials.rs".to_owned(),
            "pub url: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/credentials.rs".to_owned(),
            "token: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/credentials.rs".to_owned(),
            "value: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/sections.rs".to_owned(),
            "pub chrome_args: Vec<String>,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/sections.rs".to_owned(),
            "pub endpoint: Option<String>,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/config/sections.rs".to_owned(),
            "pub eval_script: Option<String>,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/discovery.rs".to_owned(),
            "pub value: String,".to_owned(),
        ),
        (
            "crates/crawlberg/src/types/results.rs".to_owned(),
            "pub headers: HashMap<Box<str>, Box<str>>,".to_owned(),
        ),
    ];
    expected.sort_unstable();

    assert_eq!(reviewed_sensitive_declarations(&repo_root), expected);
}

pub fn args(existing: &[String]) -> Vec<String> {
    isolate(existing, cfg!(target_os = "macos"))
}

fn isolate(existing: &[String], macos: bool) -> Vec<String> {
    if !macos {
        return existing.to_vec();
    }
    let mut args = Vec::new();
    let mut disabled = Vec::new();
    for arg in existing {
        if let Some(features) = arg.strip_prefix("--disable-features=") {
            disabled.extend(features.split(',').filter(|feature| !feature.is_empty()));
        } else {
            args.push(arg.clone());
        }
    }
    // ~keep Chrome 156's OTR key garbage collection enumerates the macOS keychain at shutdown,
    // ~keep even with --use-mock-keychain. Lifecycle fixtures must avoid creating native auth
    // ~keep keys as well as their GC; production authentication defaults remain unchanged.
    // ~keep https://chromium.googlesource.com/chromium/src/+/d85ebf44b5e14f14e65de57d0019fb04099c66af
    for feature in [
        "DeviceBoundSessions",
        "EnableBoundSessionCredentials",
        "EnableChromeRefreshTokenBinding",
        "UnexportableKeyDeletion",
    ] {
        if !disabled.contains(&feature) {
            disabled.push(feature);
        }
    }
    args.push(format!("--disable-features={}", disabled.join(",")));
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_keep_other_platform_chrome_flags_unchanged() {
        let existing = vec!["--disable-features=TranslateUI".to_owned(), "--lang=en_US".to_owned()];
        assert_eq!(isolate(&existing, false), existing);
    }

    #[test]
    fn should_merge_macos_native_key_isolation_with_existing_feature_policy() {
        let existing = vec![
            "--disable-features=TranslateUI,OtherFeature".to_owned(),
            "--lang=en_US".to_owned(),
            "--disable-features=AnotherFeature".to_owned(),
        ];
        assert_eq!(
            isolate(&existing, true),
            [
                "--lang=en_US",
                concat!(
                    "--disable-features=TranslateUI,OtherFeature,AnotherFeature,DeviceBoundSessions,",
                    "EnableBoundSessionCredentials,EnableChromeRefreshTokenBinding,UnexportableKeyDeletion"
                ),
            ]
        );
    }
}

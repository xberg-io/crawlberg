pub(super) fn metadata(agent: &str) -> Option<chromiumoxide::cdp::browser_protocol::emulation::UserAgentMetadata> {
    use chromiumoxide::cdp::browser_protocol::emulation::{UserAgentBrandVersion, UserAgentMetadata};
    let version = agent.split("Chrome/").nth(1)?.split_whitespace().next()?;
    let major = version.split('.').next()?;
    major.parse::<u32>().ok()?;
    if !version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let platform = if agent.contains("Android") {
        "Android"
    } else if agent.contains("Windows") {
        "Windows"
    } else if agent.contains("Macintosh") {
        "macOS"
    } else if agent.contains("Linux") {
        "Linux"
    } else {
        return None;
    };
    let brands = |version: &str, grease_version: &str| {
        vec![
            UserAgentBrandVersion::new("Chromium", version),
            UserAgentBrandVersion::new("Google Chrome", version),
            UserAgentBrandVersion::new("Not_A Brand", grease_version),
        ]
    };
    Some(UserAgentMetadata {
        brands: Some(brands(major, "99")),
        full_version_list: Some(brands(version, "99.0.0.0")),
        platform: platform.into(),
        platform_version: String::new(),
        architecture: if platform == "Android" || agent.contains("arm") || agent.contains("aarch64") {
            "arm"
        } else {
            "x86"
        }
        .into(),
        model: String::new(),
        mobile: agent.contains("Mobile"),
        bitness: Some(
            if platform == "macOS" || agent.contains("64") {
                "64"
            } else {
                "32"
            }
            .into(),
        ),
        wow64: Some(agent.contains("WOW64")),
        form_factors: None,
    })
}

#[cfg(test)]
mod user_agent_metadata_tests {
    #[test]
    fn should_match_chrome_version_and_linux_platform() {
        let metadata =
            super::metadata("Mozilla/5.0 (X11; Linux x86_64) Chrome/116.0.0.0 Safari/537.36").expect("Chrome metadata");
        assert_eq!(metadata.platform, "Linux");
        assert_eq!(metadata.architecture, "x86");
        assert!(!metadata.mobile);
        let brands = metadata.brands.expect("brands");
        assert_eq!(
            brands
                .iter()
                .find(|brand| brand.brand == "Chromium")
                .expect("Chromium")
                .version,
            "116"
        );
    }

    #[test]
    fn should_not_invent_chrome_hints_for_a_non_chrome_agent() {
        assert!(super::metadata("Mozilla/5.0 Firefox/128.0").is_none());
        assert!(super::metadata("Chrome/invalid").is_none());
    }
}

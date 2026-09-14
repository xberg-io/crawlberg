//! Chrome command-line flag normalisation shared by the browser-pool and interact launchers.

/// Strip the leading `--` from a Chrome flag before handing it to chromiumoxide.
///
/// ~keep chromiumoxide's `BrowserConfig::arg` stores the whole string as the key and renders it
/// as `format!("--{key}")` (chromiumoxide-0.9.1 `browser/argument.rs:35`), so an
/// already-`--`-prefixed flag emits `----disable-dev-shm-usage`, which Chrome ignores as
/// unknown. Confirmed against a launched browser's own argv: 21 flags carried four dashes, so
/// every entry of `safe_default_args`, every caller-supplied `chrome_args` entry and the
/// interact path's `--proxy-server` were silently inert. `--disable-dev-shm-usage` is the one
/// that bites hardest -- it is the standard workaround for the small /dev/shm in CI containers,
/// where Chrome otherwise stalls or crashes.
///
/// `key=value` forms need no special handling: chromiumoxide makes the whole remainder the key,
/// so `disable-features=TranslateUI` renders back as `--disable-features=TranslateUI`.
pub(crate) fn chrome_arg_key(arg: &str) -> &str {
    arg.strip_prefix("--").unwrap_or(arg)
}

#[cfg(test)]
mod tests {
    use super::chrome_arg_key;

    #[test]
    fn should_strip_the_leading_double_dash_so_chromiumoxide_does_not_double_it() {
        assert_eq!(chrome_arg_key("--disable-dev-shm-usage"), "disable-dev-shm-usage");
    }

    #[test]
    fn should_keep_the_value_intact_for_key_value_flags() {
        assert_eq!(
            chrome_arg_key("--disable-features=TranslateUI"),
            "disable-features=TranslateUI"
        );
    }

    #[test]
    fn should_leave_an_unprefixed_flag_unchanged() {
        assert_eq!(chrome_arg_key("no-sandbox"), "no-sandbox");
    }

    #[test]
    fn should_strip_only_one_level_so_a_caller_supplied_flag_is_not_mangled() {
        // ~keep A single strip is the contract: chromiumoxide re-adds exactly one `--`.
        assert_eq!(chrome_arg_key("----already-broken"), "--already-broken");
    }
}

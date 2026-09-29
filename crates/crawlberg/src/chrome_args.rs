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

/// A Chrome flag as logs and `Debug` show it: a value that holds an `@`, the mark of a user
/// name or password in a URL (`--proxy-server=http://user:pass@proxy:8080`), prints as `***`
/// after the flag name.
pub(crate) fn redacted_chrome_arg(arg: &str) -> std::borrow::Cow<'_, str> {
    match arg.split_once('=') {
        Some((name, value)) if value.contains('@') => format!("{name}=***").into(),
        None if arg.contains('@') => "***".into(),
        _ => arg.into(),
    }
}

/// `Debug` view of Chrome flags, each shown by [`redacted_chrome_arg`].
pub(crate) struct RedactedChromeArgs<'a>(pub(crate) &'a [String]);

impl std::fmt::Debug for RedactedChromeArgs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|arg| redacted_chrome_arg(arg)))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{chrome_arg_key, redacted_chrome_arg};

    #[test]
    fn a_flag_value_with_userinfo_is_hidden_and_its_name_kept() {
        for (arg, shown) in [
            (
                "--proxy-server=http://operator:IMPL385-CA@proxy.test:8080",
                "--proxy-server=***",
            ),
            (
                "--proxy-server=operator:IMPL385-CA@proxy.test:8080",
                "--proxy-server=***",
            ),
            ("operator:IMPL385-CA@proxy.test", "***"),
            (
                "--proxy-server=http://proxy.test:8080",
                "--proxy-server=http://proxy.test:8080",
            ),
            ("--disable-gpu", "--disable-gpu"),
        ] {
            assert_eq!(redacted_chrome_arg(arg), shown, "{arg}");
        }
    }

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

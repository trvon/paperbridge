//! Browser selection for `papers access`: an explicit command wins over the
//! configured one, which wins over `$BROWSER`, which wins over the desktop
//! default (`xdg-open`, `open`, or the Windows URL handler).

use std::ops::Not;

/// Where a browser command came from, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserSource {
    Flag,
    Config,
    Env,
}

impl BrowserSource {
    pub fn label(self) -> &'static str {
        match self {
            BrowserSource::Flag => "--browser",
            BrowserSource::Config => "browser_command",
            BrowserSource::Env => "$BROWSER",
        }
    }
}

/// Build the argv for one browser command. The command is split on
/// whitespace and never passed through a shell; `%s` is replaced by the URL,
/// which is appended when no `%s` is present.
pub fn browser_argv(command: &str, url: &str) -> Option<Vec<String>> {
    let mut argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return None;
    }
    if argv.iter().any(|part| part.contains("%s")) {
        for part in argv.iter_mut().skip(1) {
            *part = part.replace("%s", url);
        }
    } else {
        argv.push(url.to_string());
    }
    Some(argv)
}

/// Commands to try, in order. `--browser` and `browser_command` each name
/// one command; `$BROWSER` is a colon-separated list by convention. An empty
/// result means the desktop default should be used.
pub fn browser_candidates(
    flag: Option<&str>,
    config: Option<&str>,
    env: Option<&str>,
    url: &str,
) -> Vec<(BrowserSource, Vec<String>)> {
    let single = |source: BrowserSource, raw: Option<&str>| {
        raw.map(str::trim)
            .filter(|raw| raw.is_empty().not())
            .and_then(|raw| browser_argv(raw, url))
            .map(|argv| (source, argv))
    };
    if let Some(candidate) = single(BrowserSource::Flag, flag) {
        return vec![candidate];
    }
    if let Some(candidate) = single(BrowserSource::Config, config) {
        return vec![candidate];
    }
    env.unwrap_or_default()
        .split(':')
        .filter_map(|entry| single(BrowserSource::Env, Some(entry)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.idm.oclc.org/login?url=https://doi.org/10.1/x";

    #[test]
    fn flag_wins_over_config_and_env() {
        let candidates = browser_candidates(
            Some("google-chrome-stable --new-window"),
            Some("firefox"),
            Some("chromium"),
            URL,
        );
        assert_eq!(
            candidates,
            vec![(
                BrowserSource::Flag,
                vec![
                    "google-chrome-stable".to_string(),
                    "--new-window".to_string(),
                    URL.to_string()
                ]
            )]
        );
    }

    #[test]
    fn config_wins_over_env_and_blank_values_fall_through() {
        let candidates = browser_candidates(Some("  "), Some("firefox"), Some("chromium"), URL);
        assert_eq!(candidates[0].0, BrowserSource::Config);
        assert_eq!(candidates[0].1, vec!["firefox", URL]);
    }

    #[test]
    fn env_is_a_colon_separated_list_with_percent_s() {
        let candidates = browser_candidates(None, None, Some("chromium --app=%s::firefox"), URL);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].0, BrowserSource::Env);
        assert_eq!(
            candidates[0].1,
            vec!["chromium".to_string(), format!("--app={URL}")]
        );
        assert_eq!(candidates[1].1, vec!["firefox", URL]);
    }

    #[test]
    fn nothing_configured_means_desktop_default() {
        assert!(browser_candidates(None, None, None, URL).is_empty());
        assert!(browser_candidates(None, None, Some(""), URL).is_empty());
    }

    #[test]
    fn url_is_one_argument_and_never_shell_interpreted() {
        let hostile = "https://x.example/a?b=1&c=$(id);rm -rf";
        let argv = browser_argv("firefox --new-tab", hostile).unwrap();
        assert_eq!(argv, vec!["firefox", "--new-tab", hostile]);
    }
}

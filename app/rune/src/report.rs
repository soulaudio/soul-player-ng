//! "Report a problem" plumbing for Settings → About: gather diagnostics, redact
//! the user's home-dir, assemble a Markdown issue body, and build a pre-filled
//! GitHub new-issue URL small enough to survive GitHub's logged-out login
//! redirect (it re-encodes the whole URL into `login?return_to=…` and, past
//! ~8 KB, shows "Whoops, something went wrong"), by trimming the attached log
//! tail. Nothing is sent from here: the URL opens GitHub's web form, which is the
//! user's review-and-submit step.
#![forbid(unsafe_code)]

/// The GitHub repository (owner/name) that bug reports are filed against.
pub(crate) const ISSUE_REPO: &str = "soulaudio/soul-player-ng";

/// GitHub's practical ~8192-byte request-URI cap. We budget the issue URL by its
/// LOGGED-OUT, login-redirect-wrapped length (see [`login_wrapped_len`]) against
/// this, because that wrapped form overflows first: a signed-out user is bounced
/// to `login?return_to=<the whole URL re-encoded>` and, past this cap, GitHub
/// shows "Whoops, something went wrong" instead of the form. The direct URL a
/// signed-in user opens is always shorter and well within it.
pub(crate) const MAX_URL_LEN: usize = 8192;

/// Diagnostics for a bug report. Plain data: [`gather`] fills it from the
/// environment and app state; the assembly/encoding functions below are pure.
pub(crate) struct Diagnostics {
    /// The running app version (`CARGO_PKG_VERSION`).
    pub app_version: String,
    /// OS name and version, e.g. `"Windows 11 (26200)"`.
    pub os: String,
    /// CPU architecture, e.g. `"x86_64"`.
    pub arch: String,
    /// Logical CPU count.
    pub cpu_count: usize,
    /// The selected audio output device, if known.
    pub audio_device: Option<String>,
    /// The resolved log-file path, so the user can attach the full log.
    pub log_path: Option<String>,
}

/// Replace the user's home-directory prefix with `~` everywhere it appears, so a
/// log tail pasted into a public issue doesn't leak the account name.
pub(crate) fn redact_home(text: &str, home: &str) -> String {
    if home.is_empty() {
        return text.to_string();
    }
    text.replace(home, "~")
}

/// The Markdown issue body: a description stub, an environment block, and the
/// (already-redacted) recent-log tail fenced as a code block.
pub(crate) fn issue_body(diag: &Diagnostics, log_tail: &str) -> String {
    let mut s = String::new();
    s.push_str("### Describe the problem\n\n");
    s.push_str("<!-- What happened, what you expected, and steps to reproduce if you can. -->\n\n");
    s.push_str("### Environment\n");
    s.push_str(&format!("- Soul Player: {}\n", diag.app_version));
    s.push_str(&format!("- OS: {} ({})\n", diag.os, diag.arch));
    s.push_str(&format!("- CPUs: {}\n", diag.cpu_count));
    if let Some(dev) = &diag.audio_device {
        s.push_str(&format!("- Audio output: {dev}\n"));
    }
    if !log_tail.is_empty() {
        s.push_str("\n### Recent log\n```\n");
        s.push_str(log_tail);
        s.push_str("\n```\n");
    }
    if let Some(path) = &diag.log_path {
        s.push_str(&format!(
            "\n> Full log: Settings -> About -> \"Open logs folder\", then attach the file ({path}).\n"
        ));
    }
    s
}

/// A percent-encoded, pre-filled GitHub new-issue URL that never exceeds
/// `max_len`: the log tail is trimmed oldest-line-first to fit, and dropped with
/// a note if even an empty tail won't fit.
pub(crate) fn report_url(repo: &str, diag: &Diagnostics, log_tail: &str, max_len: usize) -> String {
    let base = format!("https://github.com/{repo}/issues/new");
    let title = "Bug: ";
    let build = |tail: &str| -> String {
        let body = issue_body(diag, tail);
        format!(
            "{base}?title={}&labels=bug&body={}",
            urlencoding::encode(title),
            urlencoding::encode(&body),
        )
    };

    // Keep as many of the most-recent log lines as fit under the cap. Growing the
    // kept tail backward from the newest line lets us stop at the first overflow
    // (the fitting tail is small), rather than rebuilding the whole log each drop.
    let all: Vec<&str> = log_tail.lines().collect();
    let mut start = all.len();
    while start > 0 && login_wrapped_len(&build(&all[start - 1..].join("\n"))) <= max_len {
        start -= 1;
    }
    if start < all.len() {
        return build(&all[start..].join("\n"));
    }

    // Not even one log line fits: drop the log (with a note) and return the base.
    let no_log = build("");
    if login_wrapped_len(&no_log) <= max_len || log_tail.is_empty() {
        return no_log;
    }
    let mut b = issue_body(diag, "");
    b.push_str("\n> Log omitted (too large for the URL). Please attach the log file.\n");
    format!(
        "{base}?title={}&labels=bug&body={}",
        urlencoding::encode(title),
        urlencoding::encode(&b),
    )
}

/// The length of the URL GitHub redirects a logged-OUT user to when they open an
/// `issues/new?...` link: `https://github.com/login?return_to=<the whole issue
/// URL, percent-encoded AGAIN>`. That second encoding pass (every `%` becomes
/// `%25`, every `?`/`&`/`=` is escaped) nearly doubles the issue URL, and if the
/// result passes GitHub's ~8 KB request-URI cap GitHub serves "Whoops, something
/// went wrong" instead of the form. We budget the issue URL by THIS length, not
/// its own, so the prefilled report works whether or not the user is signed in.
fn login_wrapped_len(issue_url: &str) -> usize {
    const LOGIN: &str = "https://github.com/login?return_to=";
    LOGIN.len() + urlencoding::encode(issue_url).len()
}

/// Build [`Diagnostics`] from the environment and the current audio-output name.
pub(crate) fn gather(audio_device: Option<String>) -> Diagnostics {
    let os = os_info::get();
    Diagnostics {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        os: format!("{} {}", os.os_type(), os.version()),
        arch: std::env::consts::ARCH.to_string(),
        cpu_count: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0),
        audio_device,
        log_path: crate::logging::log_dir().map(|p| p.display().to_string()),
    }
}

/// Gather diagnostics + a home-dir-redacted recent-log tail, build the pre-filled
/// GitHub issue URL, and open it in the browser for the user to review and submit.
/// Does file I/O and spawns the browser — call it off the UI thread.
pub(crate) fn open_issue(audio_device: Option<String>) {
    let home = dirs::home_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let tail = redact_home(&crate::logging::read_recent_log(40), &home);
    let mut diag = gather(audio_device);
    diag.log_path = diag.log_path.map(|p| redact_home(&p, &home));
    let url = report_url(ISSUE_REPO, &diag, &tail, MAX_URL_LEN);
    if let Err(e) = open::that(&url) {
        tracing::error!("open issue URL failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_diag() -> Diagnostics {
        Diagnostics {
            app_version: "0.3.0".into(),
            os: "Windows 11 (26200)".into(),
            arch: "x86_64".into(),
            cpu_count: 8,
            audio_device: Some("Speakers".into()),
            log_path: Some("C:\\Users\\alice\\AppData\\Local\\soul-player\\logs".into()),
        }
    }

    #[test]
    fn redacts_the_home_dir_prefix() {
        let out = redact_home(
            "read C:\\Users\\alice\\Music\\a.flac failed",
            "C:\\Users\\alice",
        );
        assert_eq!(out, "read ~\\Music\\a.flac failed");
    }

    #[test]
    fn redacts_every_occurrence_of_home() {
        let out = redact_home("C:\\Users\\bob then C:\\Users\\bob", "C:\\Users\\bob");
        assert_eq!(out, "~ then ~");
    }

    #[test]
    fn issue_body_includes_version_os_and_the_log_tail() {
        let body = issue_body(&sample_diag(), "line one\nline two");
        assert!(body.contains("0.3.0"), "has version: {body}");
        assert!(body.contains("Windows 11"), "has OS: {body}");
        assert!(body.contains("line one\nline two"), "has log tail: {body}");
        assert!(body.contains("### Environment"), "has env heading: {body}");
    }

    #[test]
    fn report_url_targets_the_repo_new_issue_endpoint_with_labels() {
        let url = report_url("owner/repo", &sample_diag(), "short log", MAX_URL_LEN);
        assert!(
            url.starts_with("https://github.com/owner/repo/issues/new?"),
            "{url}"
        );
        assert!(url.contains("labels=bug"), "{url}");
    }

    #[test]
    fn report_url_percent_encodes_the_body() {
        let url = report_url("owner/repo", &sample_diag(), "a b", MAX_URL_LEN);
        assert!(url.contains("body="), "{url}");
        // Markdown markers are encoded (`#` -> `%23`), not left raw in the URL.
        assert!(!url.contains("### Environment"), "{url}");
        assert!(url.contains("%23"), "{url}");
    }

    #[test]
    fn report_url_survives_the_logged_out_login_redirect() {
        // A signed-OUT user who opens an `issues/new?...` link is bounced to
        // `login?return_to=<the whole issue URL, percent-encoded AGAIN>`, which
        // roughly doubles the length. That wrapped URL must stay under GitHub's
        // ~8192-byte request-URI cap, or GitHub shows "Whoops, something went
        // wrong" instead of the form. Budgeting only the direct URL (the old bug)
        // let the wrapped URL overflow; with a huge log it must still fit.
        let huge = (0..3000)
            .map(|i| format!("log line number {i} with a bit of detail"))
            .collect::<Vec<_>>()
            .join("\n");
        let url = report_url(ISSUE_REPO, &sample_diag(), &huge, MAX_URL_LEN);
        let wrapped = login_wrapped_len(&url);
        assert!(
            wrapped <= 8192,
            "logged-out wrapped URL is {wrapped} bytes, over GitHub's ~8192 cap"
        );
        assert!(
            url.contains("issues/new?"),
            "still a valid issue URL: {url}"
        );
    }

    #[test]
    fn report_url_trims_the_log_tail_to_stay_within_budget() {
        let huge = (0..3000)
            .map(|i| format!("log line number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let max = 2500;
        let url = report_url("owner/repo", &sample_diag(), &huge, max);
        // Assert the budget the product actually enforces: the logged-out, double-encoded
        // `login?return_to=` wrapped length (report_url trims against THIS, not url.len(),
        // which is ~half the size and would pass even with the real budget blown).
        let wrapped = login_wrapped_len(&url);
        assert!(
            wrapped <= max,
            "wrapped url len {wrapped} exceeds budget {max}"
        );
        assert!(
            url.contains("issues/new?"),
            "still a valid issue URL: {url}"
        );
    }
}

//! The few server-rendered Studio pages (pure): the invitation form and a
//! plain message page for sign-in outcomes. Semantic HTML only (one `h1`,
//! landmarks, labels, `lang`), no inline script or style (the strict CSP holds),
//! and every dynamic value is HTML-escaped. The form works without JavaScript
//! except for the Turnstile widget itself, which is required (AGENTS.md §6, §8).

use sws_security::validate::TURNSTILE_FIELD;

/// Turnstile action the invitation form is bound to (checked by siteverify).
pub const INVITE_ACTION: &str = "studio_invite";
/// Form field carrying the invitation code.
pub const CODE_FIELD: &str = "code";

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn page(title: &str, body: &str, head_extra: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"robots\" content=\"noindex\">\n<title>{t} | Solo Studio</title>\n{head_extra}</head>\n\
<body>\n<a href=\"#main\">Skip to content</a>\n<header><p>Solo Studio</p></header>\n\
<main id=\"main\">\n<h1>{t}</h1>\n{body}\n</main>\n<footer><p>Solo Website System</p></footer>\n</body>\n</html>\n",
        t = escape(title)
    )
}

/// The invitation form. `site_key` is the public Turnstile site key.
pub fn invite_page(site_key: &str) -> String {
    let body = format!(
        "<p>Paste the invitation code you were given, complete the check, then continue to GitHub to sign in.</p>\n\
<form method=\"post\" action=\"/auth/invitations/accept\">\n\
<p><label for=\"{CODE_FIELD}\">Invitation code</label><br>\n\
<input id=\"{CODE_FIELD}\" name=\"{CODE_FIELD}\" type=\"text\" required autocomplete=\"off\" \
spellcheck=\"false\" maxlength=\"128\" aria-describedby=\"code-help\"></p>\n\
<p id=\"code-help\">It starts with <code>swsi_</code>. It works once and expires after seven days.</p>\n\
<div class=\"cf-turnstile\" data-sitekey=\"{}\" data-action=\"{INVITE_ACTION}\" data-response-field-name=\"{TURNSTILE_FIELD}\"></div>\n\
<noscript><p>The human check needs JavaScript. Enable it to accept the invitation.</p></noscript>\n\
<p><button type=\"submit\">Continue with GitHub</button></p>\n</form>",
        escape(site_key)
    );
    page(
        "Accept a Studio invitation",
        &body,
        "<script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\" async defer></script>\n",
    )
}

/// A message page (sign-in refused, invitation invalid, ...). `message` is
/// escaped; callers pass fixed text, never request data.
pub fn message_page(title: &str, message: &str) -> String {
    page(
        title,
        &format!(
            "<p role=\"status\">{}</p>\n<p><a href=\"/auth/github/start\">Sign in with GitHub</a></p>",
            escape(message)
        ),
        "",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(
            escape("<a href='x'>&\"</a>"),
            "&lt;a href=&#39;x&#39;&gt;&amp;&quot;&lt;/a&gt;"
        );
    }

    #[test]
    fn invite_page_is_semantic_and_escapes_the_site_key() {
        let p = invite_page("1x00000000000000000000AA\"><script>");
        assert_eq!(p.matches("<h1>").count(), 1);
        for needle in [
            "<html lang=\"en\">",
            "<main",
            "<header>",
            "<footer>",
            "<label for=\"code\">",
            "aria-describedby",
            "method=\"post\"",
        ] {
            assert!(p.contains(needle), "{needle}");
        }
        assert!(!p.contains("\"><script>"), "site key must be escaped");
        assert!(!p.contains("style="), "no inline style (CSP)");
        assert_eq!(p.matches("<script").count(), 1, "only the Turnstile script");
        assert!(p.contains(&format!("data-action=\"{INVITE_ACTION}\"")));
    }

    #[test]
    fn message_page_escapes() {
        let p = message_page("Refused", "<b>no</b>");
        assert!(p.contains("&lt;b&gt;no&lt;/b&gt;"));
        assert!(!p.contains("<script"));
        assert_eq!(p.matches("<h1>").count(), 1);
    }
}

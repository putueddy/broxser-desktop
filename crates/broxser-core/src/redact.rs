//! Redaction of page addresses and page text before they leave Broxser in a
//! bug report (ADR 0024). Both are best effort: they remove what has a
//! recognizable shape, not every secret a page could print.

/// An address reduced to where it points: hierarchical addresses keep their
/// scheme, host, port and path and lose user information, query and
/// fragment, which can carry codes and tokens; other addresses (`data:`,
/// `blob:`, `about:`) keep only their scheme. Text that is no address becomes
/// empty.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once(':') else {
        return String::new();
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return String::new();
    }
    let mut shown = format!("{scheme}:");
    if let Some(rest) = rest.strip_prefix("//") {
        let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        shown.push_str("//");
        shown.push_str(host);
        shown.push_str(path);
    }
    shown
}

/// Page text with the query, fragment and user information of each HTTP(S)
/// address removed, and JWT-shaped tokens and bearer credentials replaced.
/// Anything else stays as the page wrote it.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(length) = url_length(rest) {
            out.push_str(&redact_url(&rest[..length]));
            rest = &rest[length..];
        } else if let Some(length) = jwt_length(rest) {
            out.push_str("[token removed]");
            rest = &rest[length..];
        } else if let Some(length) = bearer_length(rest) {
            out.push_str("Bearer [token removed]");
            rest = &rest[length..];
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
            // Skip the rest of a word, so tokens are only found at its start.
            if c.is_ascii_alphanumeric() {
                let word = rest
                    .find(|c: char| !c.is_ascii_alphanumeric())
                    .unwrap_or(rest.len());
                out.push_str(&rest[..word]);
                rest = &rest[word..];
            }
        }
    }
    out
}

/// Length of the HTTP(S) address at the start of `text`: it ends at
/// whitespace, a quote, an angle bracket or a closing bracket.
fn url_length(text: &str) -> Option<usize> {
    let lower = text.get(..8)?.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return None;
    }
    Some(
        text.find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(text.len()),
    )
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_')
}

/// Length of a JWT at the start of `text`: `eyJ`, then three dot-separated
/// base64url parts (the last may be empty, as with `alg: none`).
fn jwt_length(text: &str) -> Option<usize> {
    if !text.starts_with("eyJ") {
        return None;
    }
    let mut length = 0;
    for part in 0..3 {
        let run = text[length..]
            .find(|c: char| !token_char(c))
            .unwrap_or(text.len() - length);
        if part < 2 && run < 4 {
            return None;
        }
        length += run;
        if part < 2 {
            if !text[length..].starts_with('.') {
                return None;
            }
            length += 1;
        }
    }
    Some(length)
}

/// Length of `Bearer <credential>` at the start of `text`, for credentials of
/// at least eight characters.
fn bearer_length(text: &str) -> Option<usize> {
    let prefix = text.get(..7)?;
    if !prefix.eq_ignore_ascii_case("bearer ") {
        return None;
    }
    let run = text[7..]
        .find(|c: char| !(token_char(c) || matches!(c, '.' | '~' | '+' | '/' | '=')))
        .unwrap_or(text.len() - 7);
    (run >= 8).then_some(7 + run)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_keep_only_scheme_host_and_path() {
        for (url, shown) in [
            ("https://u:p@host:8443/a/b?c=d#e", "https://host:8443/a/b"),
            ("http://host", "http://host"),
            (
                "file:///home/me/site/index.html?x",
                "file:///home/me/site/index.html",
            ),
            ("data:text/html,<script>secret</script>", "data:"),
            ("blob:http://host/1234-5678", "blob:"),
            ("about:srcdoc", "about:"),
            ("", ""),
            ("no scheme here", ""),
            ("://host/x", ""),
        ] {
            assert_eq!(redact_url(url), shown, "{url}");
        }
    }

    #[test]
    fn text_loses_address_secrets_and_tokens_only() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXZhbHVl";
        assert_eq!(
            redact_text(&format!(
                "login failed for https://me:pw@auth.test/cb?code=SECRET#at=X (token {jwt}); retry at http://api.test/v1/users?key=K."
            )),
            "login failed for https://auth.test/cb (token [token removed]); retry at http://api.test/v1/users"
        );
        assert_eq!(
            redact_text("Authorization: Bearer abc.def-ghi_jkl~mno and bearer short"),
            "Authorization: Bearer [token removed] and bearer short"
        );
        // Unsigned tokens count; look-alikes inside words and short parts do not.
        assert_eq!(
            redact_text("eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0."),
            "[token removed]"
        );
        for kept in [
            "keyJabc.defgh.ijkl",
            "eyJx.y.z",
            "plain text, no secrets: user=alice id=42",
            "HTTP://",
            "ünïcödé ✓ http",
        ] {
            assert_eq!(redact_text(kept), kept, "{kept}");
        }
        assert_eq!(redact_text("see HTTPS://Host/p?q=1"), "see HTTPS://Host/p");
    }
}

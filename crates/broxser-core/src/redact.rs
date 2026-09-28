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
            // Query removal still keeps the path, which may itself contain a
            // recognizable token. Scan that retained text without URL parsing
            // so it cannot repeatedly recognize the same address.
            out.push_str(&redact_tokens(&redact_url(&rest[..length])));
            rest = &rest[length..];
        } else if let Some(length) = jwt_length(rest) {
            out.push_str("[token removed]");
            rest = &rest[length..];
        } else if let Some(length) = bearer_length(rest) {
            out.push_str("Bearer [token removed]");
            rest = &rest[length..];
        } else {
            let length = ordinary_length(rest);
            out.push_str(&rest[..length]);
            rest = &rest[length..];
        }
    }
    out
}

/// A bounded token-only pass over retained address text. `redact_url` remains
/// suitable for console locations in the UI, where paths are kept verbatim.
fn redact_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(length) = jwt_length(rest) {
            out.push_str("[token removed]");
            rest = &rest[length..];
        } else if let Some(length) = bearer_length(rest) {
            out.push_str("Bearer [token removed]");
            rest = &rest[length..];
        } else {
            let length = ordinary_length(rest);
            out.push_str(&rest[..length]);
            rest = &rest[length..];
        }
    }
    out
}

/// Skip a whole ASCII word so token look-alikes within it are left alone.
fn ordinary_length(text: &str) -> usize {
    let c = text.chars().next().unwrap();
    if c.is_ascii_alphanumeric() {
        text.find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(text.len())
    } else {
        c.len_utf8()
    }
}

/// Length of the HTTP(S) address at the start of `text`: it ends at
/// whitespace, a quote, an angle bracket or an unmatched closing bracket.
fn url_length(text: &str) -> Option<usize> {
    if !text
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        && !text
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        return None;
    }
    // IPv6 hosts and paths can contain matched brackets or parentheses.
    // Their closing delimiters belong to the address, unlike a delimiter
    // closing a surrounding list or parenthesized sentence.
    let mut brackets = 0;
    let mut parentheses = 0;
    for (index, c) in text.char_indices() {
        if c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>') {
            return Some(index);
        }
        match c {
            '[' => brackets += 1,
            ']' if brackets > 0 => brackets -= 1,
            ']' => return Some(index),
            '(' => parentheses += 1,
            ')' if parentheses > 0 => parentheses -= 1,
            ')' => return Some(index),
            _ => {}
        }
    }
    Some(text.len())
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

/// Length of `Bearer <credential>` at the start of `text`. Credentials have
/// no minimum length; whitespace may separate the scheme from the credential.
fn bearer_length(text: &str) -> Option<usize> {
    if !text.get(..6)?.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let space = text[6..]
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(text.len() - 6);
    if space == 0 {
        return None;
    }
    let start = 6 + space;
    let run = text[start..]
        .find(|c: char| !(token_char(c) || matches!(c, '.' | '~' | '+' | '/' | '=')))
        .unwrap_or(text.len() - start);
    (run > 0).then_some(start + run)
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
            "Authorization: Bearer [token removed] and Bearer [token removed]"
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

    #[test]
    fn addresses_with_ipv6_and_unicode_hosts_are_redacted_in_surrounding_text() {
        for (text, shown) in [
            ("(http://[::1]/?token=SECRET)", "(http://[::1]/)"),
            (
                "[https://user:pw@[2001:db8::1]:8443/resource?token=abc123#secret]",
                "[https://[2001:db8::1]:8443/resource]",
            ),
            (
                "{\"url\":\"http://éxample.test/callback?token=abc123\",\"id\":42}",
                "{\"url\":\"http://éxample.test/callback\",\"id\":42}",
            ),
            (
                "http://例.test/道?key=SECRET next",
                "http://例.test/道 next",
            ),
            ("<HTTPS://[::1]/p?q=secret>", "<HTTPS://[::1]/p>"),
            ("http://[::1]/path", "http://[::1]/path"),
            (
                "(https://host/photo(1).png?token=SECRET)",
                "(https://host/photo(1).png)",
            ),
            (
                "[http://[::1]/nested(a(b)c)/file?q=SECRET] trailing",
                "[http://[::1]/nested(a(b)c)/file] trailing",
            ),
        ] {
            assert_eq!(redact_text(text), shown, "{text}");
        }
    }

    #[test]
    fn bearer_credentials_are_redacted_without_length_or_single_space_assumptions() {
        for (text, shown) in [
            (
                "Authorization: Bearer s3cr3t",
                "Authorization: Bearer [token removed]",
            ),
            ("(bearer  x)", "(Bearer [token removed])"),
            (
                "{\"auth\":\"BEARER\t a.b_~+/=\",\"ok\":true}",
                "{\"auth\":\"Bearer [token removed]\",\"ok\":true}",
            ),
            ("Bearer\n\tabc, done", "Bearer [token removed], done"),
        ] {
            assert_eq!(redact_text(text), shown, "{text}");
        }
        for text in [
            "Bearer",
            "Bearer   ",
            "Bearer: text",
            "bearerish x",
            "keyBearer x",
        ] {
            assert_eq!(redact_text(text), text);
        }
        assert_eq!(
            redact_text("{\"token\":\"eyJhbGciOiJub25lIn0.eyJzdWIiOiIxIn0.\",\"ok\":true}"),
            "{\"token\":\"[token removed]\",\"ok\":true}"
        );
    }

    #[test]
    fn tokens_in_retained_url_paths_are_redacted_only_in_export_text() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJlLXZhbHVl";
        let url = format!("https://host/reset/{jwt}?secret=q");
        assert_eq!(redact_text(&url), "https://host/reset/[token removed]");
        assert_eq!(redact_url(&url), format!("https://host/reset/{jwt}"));
        assert_eq!(
            redact_text(&format!("request {url}; token {jwt}")),
            "request https://host/reset/[token removed] token [token removed]"
        );
        for kept in [
            "https://host/assets/app.js",
            "http://例.test/道/eyJx.y.z",
            "https://host/keyJabc.defgh.ijkl",
            "https://[::1]/photo(1).png",
            "https://host/Bearer next",
        ] {
            assert_eq!(redact_text(kept), kept, "{kept}");
        }
    }
}

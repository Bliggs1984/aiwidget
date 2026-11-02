//! Masks secrets in command text before it reaches the screen or the debug log.
//!
//! The widget shows what agents run, and agents sometimes put a password or API key on the
//! command line. This works from patterns, so it reduces exposure rather than guaranteeing
//! it: a secret with no telling name or shape stays visible. It would rather hide too much
//! than too little, because it only ever changes what is displayed.

const MASK: &str = "•••";

pub fn redact(text: &str) -> String {
    let s = mask_url_logins(text);
    let s = mask_headers(&s);
    let s = mask_assignments(&s);
    let s = mask_flag_values(&s);
    mask_token_shapes(&s)
}

fn is_quote(b: u8) -> bool {
    b == b'"' || b == b'\''
}

/// Does this flag or variable name say it holds a secret? `API_KEY`, `--password` and
/// `-AccessToken` do. `MAX_TOKENS`, `--author` and `PWD` (the current folder) don't.
fn is_secret_name(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split(['-', '_', '.']).filter(|p| !p.is_empty()).collect();
    if parts.as_slice() == ["pwd"] || parts.as_slice() == ["oldpwd"] {
        return raw.starts_with('-');
    }
    parts.iter().enumerate().any(|(i, part)| {
        matches!(
            *part,
            "pass" | "passphrase" | "pwd" | "credential" | "credentials" | "auth" | "authorization" | "bearer"
                | "cookie" | "sessionid"
        ) || ["token", "secret", "password", "passwd", "apikey"].iter().any(|w| part.ends_with(w))
            || (*part == "key"
                && i > 0
                && matches!(parts[i - 1], "api" | "access" | "private" | "secret" | "auth" | "signing" | "encryption" | "ssh" | "client"))
    })
}

/// `https://user:password@host` becomes `https://•••@host`.
fn mask_url_logins(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        let after = i + 3;
        out.push_str(&rest[..after]);
        rest = &rest[after..];
        let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '/' | '"' | '\'')).unwrap_or(rest.len());
        if let Some(at) = rest[..end].rfind('@') {
            out.push_str(MASK);
            rest = &rest[at..];
        }
    }
    out.push_str(rest);
    out
}

/// `Authorization: Bearer abc` becomes `Authorization: •••`, and a bare `Bearer abc` becomes
/// `Bearer •••`.
fn mask_headers(s: &str) -> String {
    const HEADERS: &[&str] = &["authorization:", "cookie:", "x-api-key:", "api-key:", "x-auth-token:"];
    let mut out = s.to_string();
    for name in HEADERS {
        out = mask_after(&out, name, |rest| {
            let b = rest.as_bytes();
            let start = b.iter().position(|c| *c != b' ').unwrap_or(b.len());
            let end = b[start..].iter().position(|c| is_quote(*c) || *c == b'\n').map_or(b.len(), |e| start + e);
            (start, end)
        });
    }
    mask_after(&out, "bearer ", |rest| {
        let b = rest.as_bytes();
        let end = b.iter().position(|c| c.is_ascii_whitespace() || is_quote(*c)).unwrap_or(b.len());
        (0, end)
    })
}

/// Replaces part of the text following each case-insensitive match of `name`. `span` gets the
/// text after the match and returns which byte range of it to mask. It must only split the
/// text at ASCII characters.
fn mask_after(s: &str, name: &str, span: impl Fn(&str) -> (usize, usize)) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    while let Some(i) = lower[pos..].find(name) {
        let after = pos + i + name.len();
        let (start, end) = span(&s[after..]);
        out.push_str(&s[pos..after + start]);
        // An empty value, or one already masked, has nothing to hide.
        if end > start && !s[after + start..].starts_with(MASK) {
            out.push_str(MASK);
            pos = after + end;
        } else {
            pos = after + start;
        }
    }
    out.push_str(&s[pos..]);
    out
}

/// The end of the value starting at `start`: its closing quote if it has one, otherwise the
/// next whitespace or, when `stops` says so, a shell or URL separator.
fn value_end(b: &[u8], start: usize, stops: bool) -> usize {
    if start < b.len() && is_quote(b[start]) {
        return b[start + 1..].iter().position(|c| *c == b[start]).map_or(b.len(), |e| start + 1 + e + 1);
    }
    let stop = |c: u8| c.is_ascii_whitespace() || (stops && (is_quote(c) || matches!(c, b'&' | b';' | b'|' | b')')));
    b[start..].iter().position(|c| stop(*c)).map_or(b.len(), |e| start + e)
}

/// `API_KEY=abc`, `--token=abc`, `$env:TOKEN = "abc"` and `?token=abc` hide the value.
fn mask_assignments(s: &str) -> String {
    let b = s.as_bytes();
    let is_name = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.');
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'=' || b.get(i + 1) == Some(&b'=') || (i > 0 && matches!(b[i - 1], b'=' | b'!' | b'<' | b'>')) {
            i += 1;
            continue;
        }
        let mut name_end = i;
        while name_end > 0 && b[name_end - 1] == b' ' {
            name_end -= 1;
        }
        let mut name_start = name_end;
        while name_start > 0 && is_name(b[name_start - 1]) {
            name_start -= 1;
        }
        let mut start = i + 1;
        while start < b.len() && b[start] == b' ' {
            start += 1;
        }
        let end = value_end(b, start, true);
        if end > start && is_secret_name(&s[name_start..name_end]) && !s[start..].starts_with(MASK) {
            out.push_str(&s[copied..start]);
            out.push_str(MASK);
            copied = end;
            i = end;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// `--password abc`, `-ApiKey abc`, `curl -u name:abc` and `sshpass -p abc` hide the value.
fn mask_flag_values(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    let mut prev: &str = "";
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let end = value_end(b, i, false);
        let word = &s[i..end];
        let mut next = end;
        while next < b.len() && b[next].is_ascii_whitespace() {
            next += 1;
        }
        let next_end = value_end(b, next, false);
        let value = &s[next..next_end];
        let hides_next = word.starts_with('-')
            && !word.contains('=')
            && !value.is_empty()
            && !value.starts_with('-')
            && !value.starts_with(MASK)
            && (is_secret_name(word)
                || (matches!(word, "-u" | "--user") && value.contains(':') && !value.contains("://"))
                || (word == "-p" && prev.ends_with("sshpass")));
        if hides_next {
            out.push_str(&s[copied..next]);
            out.push_str(MASK);
            copied = next_end;
            i = next_end;
        } else {
            i = end;
        }
        prev = word;
    }
    out.push_str(&s[copied..]);
    out
}

/// Well-known key formats, wherever they appear: (start, shortest length, letters and digits
/// only after the start). That last one stops `npm_config_registry` counting as a key.
const TOKEN_SHAPES: &[(&str, usize, bool)] = &[
    ("sk-", 20, false),
    ("ghp_", 20, false),
    ("gho_", 20, false),
    ("ghu_", 20, false),
    ("ghs_", 20, false),
    ("ghr_", 20, false),
    ("github_pat_", 20, false),
    ("glpat-", 20, false),
    ("xoxb-", 15, false),
    ("xoxp-", 15, false),
    ("xoxa-", 15, false),
    ("xapp-", 15, false),
    ("AKIA", 16, true),
    ("ASIA", 16, true),
    ("AIza", 30, false),
    ("ya29.", 20, false),
    ("hf_", 30, true),
    ("npm_", 36, true),
    ("gsk_", 20, true),
    ("pplx-", 20, true),
];

fn looks_like_key(word: &str) -> bool {
    if word.len() >= 30 && word.starts_with("eyJ") && word.contains('.') {
        return true; // a signed web token
    }
    TOKEN_SHAPES.iter().any(|(start, min_len, alnum)| {
        word.len() >= *min_len
            && word.starts_with(start)
            && (!alnum || word[start.len()..].bytes().all(|c| c.is_ascii_alphanumeric()))
    })
}

fn mask_token_shapes(s: &str) -> String {
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        let start = rest.find(is_word).unwrap_or(rest.len());
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest.find(|c| !is_word(c)).unwrap_or(rest.len());
        out.push_str(if looks_like_key(&rest[..end]) { MASK } else { &rest[..end] });
        rest = &rest[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hides(input: &str, secret: &str) {
        let out = redact(input);
        assert!(!out.contains(secret), "{input:?} became {out:?}");
        assert!(out.contains(MASK), "{input:?} became {out:?}");
    }

    #[test]
    fn hides_headers() {
        hides(r#"curl -H "Authorization: Bearer abc123def" https://x.dev"#, "abc123def");
        hides(r#"curl -H 'x-api-key: abc123def' https://x.dev"#, "abc123def");
        hides(r#"curl -H "Cookie: session=abc123def; b=c" https://x.dev"#, "abc123def");
        hides(r#"fetch(url, { "Authorization": "Bearer abc123def" })"#, "abc123def");
    }

    #[test]
    fn hides_assignments() {
        hides("API_KEY=abc123def npm start", "abc123def");
        hides("export GITHUB_TOKEN=abc123def && gh pr list", "abc123def");
        hides(r#"$env:OPENAI_API_KEY = "abc 123 def"; py x.py"#, "abc 123 def");
        hides("tool --password=abc123def --verbose", "abc123def");
        hides("DB_PASS='abc123def' psql", "abc123def");
        hides("curl https://x.dev/y?a=1&token=abc123def&b=2", "abc123def");
    }

    #[test]
    fn keeps_what_is_around_a_hidden_value() {
        assert_eq!(redact("API_KEY=abc123def npm start"), "API_KEY=••• npm start");
        assert_eq!(redact("curl https://x.dev/y?token=abc&b=2"), "curl https://x.dev/y?token=•••&b=2");
        assert_eq!(redact(r#"tool --token "a b c" --verbose"#), "tool --token ••• --verbose");
        assert_eq!(redact(r#"curl -H "Authorization: Basic abc" x"#), r#"curl -H "Authorization: •••" x"#);
    }

    #[test]
    fn hides_flag_values() {
        hides("tool --password abc123def", "abc123def");
        hides(r#"tool --api-key "abc123def""#, "abc123def");
        hides("Connect-Thing -AccessToken abc123def -Verbose", "abc123def");
        hides("curl -u brett:abc123def https://x.dev", "abc123def");
        hides("sshpass -p abc123def ssh host", "abc123def");
    }

    #[test]
    fn hides_url_logins() {
        hides("git clone https://brett:abc123def@github.com/a/b.git", "abc123def");
        assert_eq!(redact("https://a:b@x.dev/path"), "https://•••@x.dev/path");
    }

    #[test]
    fn hides_well_known_key_formats_anywhere() {
        hides("py x.py sk-ant-abcdefghijklmnopqrstuv", "sk-ant-abc");
        hides("echo ghp_abcdefghijklmnopqrstuvwxyz0123", "ghp_abc");
        hides("deploy AKIAABCDEFGHIJKLMNOP now", "AKIAABC");
        hides("x eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc", "eyJhbGci");
    }

    /// Ordinary commands must stay readable: that is the point of the widget.
    #[test]
    fn leaves_ordinary_commands_alone() {
        for cmd in [
            "cargo test --release",
            "git push -u origin main",
            "git log --author=Brett --oneline",
            "rg --max-tokens 5 tokenizer src",
            "MAX_TOKENS=100 py run.py",
            "PWD=/c/Utils py run.py",
            "sort --key=2 file.txt",
            "npm_config_registry=https://x.dev npm i",
            "cd /tmp/sk-learn && pip install scikit-learn",
            "if [ \"$a\" == \"$b\" ]; then echo same; fi",
            "ssh git@github.com",
            "cargo build -p aiwidget",
            "npm run task-management-dashboard-long-name",
            "echo héllo wörld",
        ] {
            assert_eq!(redact(cmd), cmd);
        }
    }

    #[test]
    fn running_it_twice_changes_nothing() {
        for cmd in [
            r#"curl -H "Authorization: Bearer abc" https://a:b@x.dev?token=c --password d"#,
            "API_KEY=abc --token def sk-abcdefghijklmnopqrstuvwxyz",
        ] {
            let once = redact(cmd);
            assert_eq!(redact(&once), once);
        }
    }

    #[test]
    fn copes_with_odd_text() {
        for cmd in ["", "=", "==", "a=", "token=", "--token", "--token ", "token=\"unclosed", "://", "://@", "é=é", "Authorization:", "bearer ", "–token é"] {
            let _ = redact(cmd);
        }
        hides("token=\"unclosed secret", "unclosed secret");
        hides("tool --password é€abc", "é€abc");
    }
}

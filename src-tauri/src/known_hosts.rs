//! Parse and match OpenSSH `known_hosts` files.
//!
//! Fingerprints default to TOFU, but keys can also be pinned before the first
//! connect; the trust set can be imported from `ssh-keyscan` output or an
//! existing `~/.ssh/known_hosts`, and exported for `ssh` or backups.
//!
//! We do not reuse `russh::keys::known_hosts`: it takes file paths only, knows
//! no `*`/`?` globs or `!` negation, and mistakes a `@cert-authority` CA record
//! for a host key. This module parses with `ssh_key` and HMAC-SHA1; matching
//! rules are documented on [`lookup`].

use hmac::{Hmac, KeyInit, Mac};
use russh::keys::ssh_key::{
    self,
    encoding::base64::{Base64, Encoding},
    HashAlg, PublicKey,
};
use sha1::Sha1;

/// Default port. `known_hosts` rules: non-22 ports are written `[host]:port`,
/// and entries without a port apply to port 22 only.
pub const DEFAULT_PORT: u16 = 22;

/// Line marker starting with `@`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// `@revoked`: the key is explicitly revoked, so always reject it.
    Revoked,
    /// `@cert-authority`: a CA that signs host certificates, not a host key.
    /// Host certificates are unsupported, so this is never used as a host key.
    CertAuthority,
}

impl Marker {
    pub fn as_str(self) -> &'static str {
        match self {
            Marker::Revoked => "@revoked",
            Marker::CertAuthority => "@cert-authority",
        }
    }
}

/// One entry.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Line number (1-based), used to report which line is bad.
    pub line: usize,
    /// Host patterns as written: `a,b` or `|1|salt|hash`.
    pub patterns: String,
    pub marker: Option<Marker>,
    pub key: PublicKey,
    /// Standard form such as `ssh-ed25519 AAAA...` (used when exporting).
    pub key_line: String,
    /// `SHA256:...`, the same form as `Client::host_key`.
    pub fingerprint: String,
}

impl Entry {
    /// Human-readable name this entry is for.
    pub fn label(&self) -> String {
        if let Some(marker) = self.marker {
            format!("{} {}", marker.as_str(), self.patterns)
        } else {
            self.patterns.clone()
        }
    }
}

/// Parse result. Unreadable lines do not stop parsing, they are recorded:
/// the input may be a whole `known_hosts` with a few odd lines.
#[derive(Debug, Default)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    /// `line N: reason`
    pub problems: Vec<String>,
}

/// Parse a `known_hosts` text.
pub fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        let (marker, fields) = match fields.split_first() {
            Some((first, rest)) if first.starts_with('@') => match *first {
                "@revoked" => (Some(Marker::Revoked), rest),
                "@cert-authority" => (Some(Marker::CertAuthority), rest),
                other => {
                    parsed
                        .problems
                        .push(format!("第 {line} 行:不认识的标记 {other},已跳过"));
                    continue;
                }
            },
            Some((_, _)) => (None, &fields[..]),
            None => continue,
        };

        if fields.len() < 2 {
            parsed
                .problems
                .push(format!("第 {line} 行:字段不够(至少要 `主机 钥匙`)"));
            continue;
        }
        let (patterns, key_type, key_b64) = (fields[0], fields[1], fields[2..].join(" "));

        // SSH1-era `<host> <bits> <exponent> <modulus>`: recognized but unsupported
        if key_type.chars().all(|c| c.is_ascii_digit()) {
            parsed.problems.push(format!(
                "第 {line} 行:这是 SSH1 时代的 RSA1 格式,早就不用了,不支持"
            ));
            continue;
        }
        if key_b64.trim().is_empty() {
            parsed
                .problems
                .push(format!("第 {line} 行:{key_type} 后面没有钥匙内容"));
            continue;
        }

        // Hand only `type base64` to ssh_key, without any trailing comment
        let openssh = format!("{} {}", key_type, key_b64.split_whitespace().next().unwrap_or(""));
        let key = match PublicKey::from_openssh(&openssh) {
            Ok(key) => key,
            Err(err) => {
                parsed
                    .problems
                    .push(format!("第 {line} 行:解不出公钥({err})"));
                continue;
            }
        };

        parsed.entries.push(Entry {
            line,
            patterns: patterns.to_string(),
            marker,
            key_line: key.to_openssh().unwrap_or(openssh),
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            key,
        });
    }

    parsed
}

/// Which key to use for a host, as chosen from the parse result.
#[derive(Debug, Default)]
pub struct Lookup<'a> {
    /// Trustworthy candidates (one key can be hit by several entries).
    pub candidates: Vec<&'a Entry>,
    /// Hit `@revoked` entries: explicitly revoked, never trusted.
    pub revoked: Vec<&'a Entry>,
    /// Hit `@cert-authority` entries: a CA, not a host key.
    pub authorities: Vec<&'a Entry>,
    /// Notes shown to the user (non-blocking, but worth saying).
    pub notes: Vec<String>,
}

impl Lookup<'_> {
    /// Whether all candidates are the same key. Several different keys at once
    /// must be decided by a human.
    pub fn agrees(&self) -> bool {
        self.candidates
            .iter()
            .map(|entry| entry.fingerprint.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            <= 1
    }

    pub fn fingerprints(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        self.candidates
            .iter()
            .filter(|entry| seen.insert(entry.fingerprint.clone()))
            .map(|entry| entry.fingerprint.clone())
            .collect()
    }
}

/// Pick the entries that apply to `host:port` from a parse result.
///
/// Matching follows OpenSSH:
///
/// * comma-separated and possibly repeated; `*`/`?` globs; a `!` prefix negates
///   (a hit voids the whole line);
/// * `[host]:port` applies to that port only; **entries without a port apply to
///   port 22 only** (easily missed: a file with plain `host` entries will match
///   nothing on port 2222, so a note is emitted);
/// * `|1|salt|hash` is a hashed hostname, compared by HMAC-SHA1(salt, `host` or
///   `[host]:port`).
///
/// Hostnames compare **case-insensitively** (as DNS does); OpenSSH also only
/// lowercases the hostname, so `Host.Example.COM` still matches.
pub fn lookup<'a>(parsed: &'a Parsed, host: &str, port: u16) -> Lookup<'a> {
    let mut result = Lookup::default();
    let mut matched_plain_on_wrong_port = false;

    for entry in &parsed.entries {
        let hit = if entry.patterns.starts_with("|1|") {
            hashed_matches(&entry.patterns, host, port)
        } else {
            patterns_match(&entry.patterns, host, port)
        };
        if !hit {
            // On a non-22 port, note that the host is present but its entry is
            // for port 22 only
            if port != DEFAULT_PORT
                && !entry.patterns.starts_with("|1|")
                && patterns_match(&entry.patterns, host, DEFAULT_PORT)
            {
                matched_plain_on_wrong_port = true;
            }
            continue;
        }

        match entry.marker {
            Some(Marker::Revoked) => result.revoked.push(entry),
            Some(Marker::CertAuthority) => result.authorities.push(entry),
            None => result.candidates.push(entry),
        }
    }

    // Say this only when nothing matched at all; with a usable entry present,
    // explaining why other lines did not match only adds noise.
    if matched_plain_on_wrong_port
        && result.candidates.is_empty()
        && result.revoked.is_empty()
        && result.authorities.is_empty()
    {
        result.notes.push(format!(
            "文件里有 {host} 的记录,但你连的是 {port} 端口 —— known_hosts 的规矩是\
             不带端口的记录只对 22 端口生效,那种情况要写成 [{host}]:{port}"
        ));
    }
    if !result.authorities.is_empty() {
        result.notes.push(
            "命中了 @cert-authority:那是给主机签证书的 CA,不是主机自己的钥匙。\
             本工具不支持主机证书,这条记录被忽略了"
                .to_string(),
        );
    }

    result
}

/// Comma-separated patterns on one line (with `!` negation).
fn patterns_match(patterns: &str, host: &str, port: u16) -> bool {
    let mut matched = false;
    for pattern in patterns.split(',') {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        if pattern_match(pattern, host, port) {
            if negated {
                // Negated hit -> the whole entry is void (OpenSSH semantics)
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// One pattern: `host`, `[host]:port`, or a glob host.
fn pattern_match(pattern: &str, host: &str, port: u16) -> bool {
    if let Some(rest) = pattern.strip_prefix('[') {
        let Some((name, port_text)) = rest.split_once("]:") else {
            return false;
        };
        let Ok(wanted) = port_text.parse::<u16>() else {
            return false;
        };
        return wanted == port && glob_match(name, host);
    }
    port == DEFAULT_PORT && glob_match(pattern, host)
}

/// `*` matches any length, `?` matches one char; the rest is literal
/// (case-insensitive).
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();

    // Classic two-pointer backtracking, `*` remembers only its last position:
    // no regex, no exponential behavior
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut resume) = (None, 0usize);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            resume = t;
            p += 1;
        } else if let Some(at) = star {
            p = at + 1;
            resume += 1;
            t = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// `|1|salt|hash` form: compare HMAC-SHA1(salt, name) against hash.
fn hashed_matches(patterns: &str, host: &str, port: u16) -> bool {
    let parts: Vec<&str> = patterns.split('|').collect();
    // ["", "1", salt, hash]
    if parts.len() != 4 {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (Base64::decode_vec(parts[2]), Base64::decode_vec(parts[3]))
    else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) else {
        return false;
    };
    mac.update(name_for(host, port).as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// Name used for lookup / hashing: non-22 ports are written `[host]:port`.
pub fn name_for(host: &str, port: u16) -> String {
    if port == DEFAULT_PORT {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Build one `known_hosts` line (safe to append to `~/.ssh/known_hosts`).
pub fn line_for(host: &str, port: u16, key_line: &str) -> String {
    format!("{} {}", name_for(host, port), key_line.trim())
}

/// Line for a fingerprint without the public key: standard `known_hosts` has no
/// such form, so it is prefixed with `#` as a comment — still readable for
/// backup/comparison, and never misused by other tools.
pub fn comment_line_for(host: &str, port: u16, fingerprint: &str) -> String {
    format!("# {} {fingerprint} (只有指纹,重新连接一次才能拿到公钥原文)", name_for(host, port))
}

/// `ssh-key` error type, re-exported so callers need fewer imports.
pub type KeyError = ssh_key::Error;

#[cfg(test)]
mod tests {
    use super::*;

    // These public keys are **test keys** copied from russh's own tests, not
    // from any real machine.
    const KEY_A: &str = "AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF";
    const KEY_B: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_C: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";

    /// Hashed hostnames: `salt` is fixed 00..09, the hash was computed
    /// independently with .NET HMACSHA1 (not with our own code, which would be
    /// checking ourselves against ourselves).
    const SALT_B64: &str = "AAECAwQFBgcICQ==";
    const HASH_EXAMPLE_COM_22: &str = "aj4y4B1XaRyUuaBo42Cp74iYDXk=";
    const HASH_EXAMPLE_COM_2222: &str = "MzK8soy4EFk6wWfNnOh8us+JjKQ=";
    const HASH_EXAMPLE_ORG_22: &str = "UJWLyVvg68/v2QNywDL+11DQMJg=";

    fn sample() -> String {
        format!(
            "# 注释行\n\
             \n\
             example.com ssh-ed25519 {KEY_A}\n\
             [jump.example.com]:2222 ssh-ed25519 {KEY_B}\n\
             pijul.org,37.120.161.53 ssh-ed25519 {KEY_C}\n\
             |1|{SALT_B64}|{HASH_EXAMPLE_COM_22} ssh-ed25519 {KEY_A}\n\
             |1|{SALT_B64}|{HASH_EXAMPLE_COM_2222} ssh-ed25519 {KEY_B}\n\
             |1|{SALT_B64}|{HASH_EXAMPLE_ORG_22} ssh-ed25519 {KEY_C}\n\
             @revoked dead.example.com ssh-ed25519 {KEY_A}\n\
             @cert-authority *.corp.example.com ssh-ed25519 {KEY_B}\n\
             *.wild.example.com ssh-ed25519 {KEY_C}\n\
             !secret.example.com,*.example.com ssh-ed25519 {KEY_B}\n\
             this line is garbage\n\
             192.0.2.1 1024 37 12345\n"
        )
    }

    #[test]
    fn parses_a_realistic_file_and_reports_what_it_cannot_read() {
        let parsed = parse(&sample());
        // 10 readable entries (comments, blank lines and two garbage lines excluded)
        assert_eq!(parsed.entries.len(), 10, "{:#?}", parsed.problems);
        assert_eq!(parsed.problems.len(), 2, "{:#?}", parsed.problems);
        assert!(parsed.problems[0].contains("第 13 行"), "{:?}", parsed.problems);
        assert!(parsed.problems[1].contains("RSA1"), "{:?}", parsed.problems);

        let first = &parsed.entries[0];
        assert_eq!(first.line, 3);
        assert_eq!(first.patterns, "example.com");
        assert!(first.fingerprint.starts_with("SHA256:"), "{}", first.fingerprint);
        assert!(first.key_line.starts_with("ssh-ed25519 "), "{}", first.key_line);

        let revoked = parsed
            .entries
            .iter()
            .find(|entry| entry.marker == Some(Marker::Revoked))
            .expect("expected an @revoked entry");
        assert_eq!(revoked.patterns, "dead.example.com");
        assert_eq!(revoked.label(), "@revoked dead.example.com");

        let authority = parsed
            .entries
            .iter()
            .find(|entry| entry.marker == Some(Marker::CertAuthority))
            .expect("expected an @cert-authority entry");
        assert_eq!(authority.patterns, "*.corp.example.com");
    }

    #[test]
    fn plain_entries_only_apply_to_the_default_port() {
        let parsed = parse(&sample());

        // Port 22: a direct hit on example.com plus the hashed entry
        let found = lookup(&parsed, "example.com", 22);
        assert_eq!(found.candidates.len(), 2, "{:?}", found.fingerprints());
        assert!(found.agrees(), "both entries point at the same key");
        assert!(found.revoked.is_empty());
        assert_eq!(found.candidates[0].key_line, format!("ssh-ed25519 {KEY_A}"));

        // Port 2222: the `[jump.example.com]:2222` entry counts
        let found = lookup(&parsed, "jump.example.com", 2222);
        assert_eq!(found.candidates.len(), 1);
        assert_eq!(found.candidates[0].key_line, format!("ssh-ed25519 {KEY_B}"));

        // On port 22 it is hit only by the wildcard entry (no `jump.example.com`
        // line exists in the file)
        let found = lookup(&parsed, "jump.example.com", 22);
        assert_eq!(found.candidates.len(), 1);
        assert_eq!(found.candidates[0].patterns, "!secret.example.com,*.example.com");
    }

    /// A portless entry against a non-22 port: no match, but say **why**.
    ///
    /// Use a name no wildcard entry covers, otherwise another entry would match
    /// and the note would correctly not appear.
    #[test]
    fn warns_when_a_plain_entry_is_used_on_a_custom_port() {
        let parsed = parse(&sample());
        let found = lookup(&parsed, "pijul.org", 2222);
        assert!(found.candidates.is_empty(), "{:?}", found.fingerprints());
        assert_eq!(found.notes.len(), 1, "{:?}", found.notes);
        assert!(found.notes[0].contains("[pijul.org]:2222"), "{:?}", found.notes);

        // With a matching entry already present, stay quiet
        assert!(lookup(&parsed, "example.com", 2222).notes.is_empty());
    }

    #[test]
    fn hashed_hostnames_need_hmac_sha1() {
        let parsed = parse(&sample());

        // The hashed entry for port 22 hits example.com (same key as the plain one)
        let hit = lookup(&parsed, "example.com", 22);
        assert_eq!(hit.candidates.len(), 2, "{:?}", hit.fingerprints());
        // The non-22 entry applies to 2222 only
        let hit = lookup(&parsed, "example.com", 2222);
        assert_eq!(hit.candidates.len(), 1);
        assert_eq!(hit.candidates[0].key_line, format!("ssh-ed25519 {KEY_B}"));

        // The other hashed entry is for example.org: same salt, different hash;
        // hitting it proves HMAC is really compared, not just "a hash exists"
        let org = lookup(&parsed, "example.org", 22);
        assert_eq!(org.candidates.len(), 1, "{:?}", org.fingerprints());
        assert_eq!(org.candidates[0].key_line, format!("ssh-ed25519 {KEY_C}"));

        // A name matching nothing must match nothing
        assert!(lookup(&parsed, "example.net", 22).candidates.is_empty());
        assert!(lookup(&parsed, "example.net", 2222).candidates.is_empty());
    }

    #[test]
    fn wildcards_negation_and_revocation() {
        let parsed = parse(&sample());

        // `a.wild.example.com` is hit by both `*.wild.example.com` and `*.example.com`
        let wild = lookup(&parsed, "a.wild.example.com", 22);
        assert_eq!(wild.candidates.len(), 2, "{:?}", wild.candidates.iter().map(|e| &e.patterns).collect::<Vec<_>>());

        // In `*,*` the name `secret.example.com` hits the negation -> entry void
        let negated = lookup(&parsed, "secret.example.com", 22);
        assert!(negated.candidates.is_empty(), "{:?}", negated.fingerprints());
        assert!(negated.notes.is_empty());

        // @revoked: must be recognized and must **not** leak into candidates.
        // (`dead.example.com` is also covered by the wildcard entry, so it
        //  appears among candidates -- that is another key; OpenSSH `@revoked`
        //  revokes only its own key, not the others.)
        let revoked = lookup(&parsed, "dead.example.com", 22);
        assert_eq!(revoked.revoked.len(), 1);
        assert_eq!(
            revoked.revoked[0].fingerprint,
            parsed
                .entries
                .iter()
                .find(|entry| entry.patterns == "example.com")
                .unwrap()
                .fingerprint
        );
        assert!(
            revoked
                .candidates
                .iter()
                .all(|entry| entry.patterns != "dead.example.com"),
            "the @revoked entry must not show up as a candidate"
        );

        // @cert-authority: never a host key, and must be reported clearly.
        // (Again the wildcard entry still counts -- CA is CA, host keys are host keys)
        let authority = lookup(&parsed, "ci.corp.example.com", 22);
        assert_eq!(authority.authorities.len(), 1);
        assert!(
            authority
                .candidates
                .iter()
                .all(|entry| entry.marker.is_none()),
            "marked entries must not leak into candidates"
        );
        assert_eq!(authority.notes.len(), 1);
        assert!(authority.notes[0].contains("cert-authority"));
    }

    /// The `!secret.example.com,*.example.com` entry: negation kills only its
    /// own pattern; other patterns on the line still apply.
    #[test]
    fn negation_only_kills_its_own_pattern() {
        let parsed = parse(&sample());
        let inside = lookup(&parsed, "other.example.com", 22);
        assert_eq!(inside.candidates.len(), 1);
        assert_eq!(inside.candidates[0].patterns, "!secret.example.com,*.example.com");
    }

    /// Plain patterns compare case-insensitively (as DNS does).
    ///
    /// **Hashed entries do not**: their hash was computed from the name written
    /// at the time, so `EXAMPLE.com` cannot match a hash made for `example.com`.
    /// That is not a bug but inherent to HMAC; it does mean a renamed entry
    /// silently stops matching.
    #[test]
    fn plain_hostnames_are_case_insensitive_but_hashed_ones_are_not() {
        let parsed = parse(&sample());
        let upper = lookup(&parsed, "EXAMPLE.com", 22);
        assert_eq!(upper.candidates.len(), 1, "only the plain entry matches");
        assert_eq!(upper.candidates[0].patterns, "example.com");

        assert_eq!(lookup(&parsed, "example.com", 22).candidates.len(), 2);
        // Wildcards are case-insensitive too
        assert_eq!(lookup(&parsed, "a.WILD.example.COM", 22).candidates.len(), 2);
    }

    #[test]
    fn glob_handles_the_nasty_cases() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("[::1]", "[::1]"));
        assert!(glob_match("host", "HOST"));
        assert!(!glob_match("", "x"));
        assert!(glob_match("", ""));
        // A run of `*` must not blow up (backtracking needs one star only)
        assert!(glob_match("**********a", "aaaaaaaaaa"));
    }

    #[test]
    fn line_for_writes_what_ssh_expects() {
        assert_eq!(
            line_for("example.com", 22, &format!("ssh-ed25519 {KEY_A}")),
            format!("example.com ssh-ed25519 {KEY_A}")
        );
        assert_eq!(
            line_for("example.com", 2222, &format!("ssh-ed25519 {KEY_A}  ")),
            format!("[example.com]:2222 ssh-ed25519 {KEY_A}")
        );
        // An exported line must parse back to the same key (a round trip proves the format)
        let text = line_for("example.com", 2222, &format!("ssh-ed25519 {KEY_A}"));
        let parsed = parse(&text);
        assert_eq!(parsed.entries.len(), 1);
        let back = lookup(&parsed, "example.com", 2222);
        assert_eq!(back.candidates.len(), 1);
        assert_eq!(back.candidates[0].key_line, format!("ssh-ed25519 {KEY_A}"));
    }

    #[test]
    fn comment_line_keeps_a_fingerprint_readable() {
        let line = comment_line_for("example.com", 2222, "SHA256:abc");
        assert!(line.starts_with("# [example.com]:2222 SHA256:abc"));
        // It is a comment, so parsing it back yields nothing
        assert!(parse(&line).entries.is_empty());
    }

    const KEY_B_ALIAS: &str = KEY_B;
    #[test]
    fn two_different_keys_on_the_same_host_do_not_agree() {
        // Hand-build a file with "two different keys for one machine" (what a
        // changed host key looks like in practice)
        let text = format!(
            "example.com ssh-ed25519 {KEY_A}\nexample.com ssh-ed25519 {KEY_ALIAS}\n",
            KEY_ALIAS = KEY_B_ALIAS
        );
        let parsed = parse(&text);
        let found = lookup(&parsed, "example.com", 22);
        assert_eq!(found.candidates.len(), 2);
        assert!(!found.agrees(), "two different keys must not count as agreeing");
        assert_eq!(found.fingerprints().len(), 2);
    }
}

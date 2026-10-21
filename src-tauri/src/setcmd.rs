//! Parsing for `set ...`: turns **categorized** settings into a flat `POST /api/config` patch.
//!
//! A separate module because two frontends share it: the **CLI window** (the app itself,
//! `src/cli.rs`, log pane + input line) and the **standalone CLI** (`compilegateway-cli`,
//! the thin client from design doc §3). Only the latter used to have `set`, so typing `set`
//! in the window returned `unknown command` (the bug users reported).
//! The result mirrors the WebUI settings page one-to-one: ports split WebUI / MCP,
//! credentials split username / password, host roots split add / remove.

use serde_json::{json, Map, Value};

/// The parsed intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Values to change (posted straight to `/api/config`).
    Patch(Value),
    /// No change, just the full settings view (bare `set` / `set all`).
    Overview,
    /// Show the host-root allowlist (`set roots` / `set roots list`).
    RootsList,
}

/// Why parsing failed. **Structured** so frontends can localize it (`message()` is the Chinese fallback).
#[derive(Debug, Clone, PartialEq)]
pub enum SetError {
    UnknownCategory(String),
    /// `port` / `listen` did not say which side (WebUI or MCP).
    MissingTarget,
    /// No value followed `--webui` / `--mcp`.
    MissingValue(String),
    BadValue {
        what: String,
        value: String,
        want: &'static str,
    },
    /// `user` gave none of its fields.
    NothingToChange,
    /// No path followed `roots add|rm|set`.
    NeedPath(String),
    UnknownAction {
        category: String,
        action: String,
    },
}

impl SetError {
    pub fn message(&self) -> String {
        let lang = crate::i18n::current();
        match self {
            SetError::UnknownCategory(name) => crate::i18n::set_unknown_category(lang, name),
            SetError::MissingTarget => crate::i18n::set_missing_target(lang),
            SetError::MissingValue(flag) => crate::i18n::set_missing_value(lang, flag),
            SetError::BadValue { what, value, want } => {
                crate::i18n::set_bad_value(lang, what, value, want)
            }
            SetError::NothingToChange => crate::i18n::set_nothing_to_change(lang),
            SetError::NeedPath(action) => crate::i18n::set_need_path(lang, action),
            SetError::UnknownAction { category, action } => {
                crate::i18n::set_unknown_action(lang, category, action)
            }
        }
    }
}

/// Split on whitespace, supporting `"paths with spaces"` and `'single quotes'` (Windows paths often contain spaces).
pub fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in input.chars() {
        match quote {
            Some(mark) if ch == mark => quote = None,
            Some(_) => current.push(ch),
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            None => current.push(ch),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Parse the tokens after `set` (`tokens[0]` is the category).
pub fn parse(tokens: &[String]) -> Result<Intent, SetError> {
    let Some(category) = tokens.first().map(String::as_str) else {
        return Ok(Intent::Overview);
    };
    let args = &tokens[1..];
    let value_of = |flag: &str| -> Option<String> {
        // Accepts both `--webui 1234` and `--webui=1234`
        let mut index = 0;
        while index < args.len() {
            let token = &args[index];
            if let Some(rest) = token.strip_prefix(&format!("{flag}=")) {
                return Some(rest.to_string());
            }
            if token == flag {
                return args.get(index + 1).cloned();
            }
            index += 1;
        }
        None
    };
    let has = |flag: &str| args.iter().any(|token| token == flag);

    let mut patch = Map::new();
    match category {
        "port" => {
            let mut touched = false;
            if let Some(raw) = value_of("--webui") {
                patch.insert("port".into(), json!(port(&raw, "--webui")?));
                touched = true;
            } else if has("--webui") {
                return Err(SetError::MissingValue("--webui".into()));
            }
            if let Some(raw) = value_of("--mcp") {
                patch.insert("mcp".into(), json!({ "port": port(&raw, "--mcp")? }));
                touched = true;
            } else if has("--mcp") {
                return Err(SetError::MissingValue("--mcp".into()));
            }
            if !touched {
                return Err(SetError::MissingTarget);
            }
        }
        "listen" => {
            let mut touched = false;
            if let Some(raw) = value_of("--webui") {
                patch.insert("listen".into(), json!(listen(&raw)?));
                touched = true;
            } else if has("--webui") {
                return Err(SetError::MissingValue("--webui".into()));
            }
            if let Some(raw) = value_of("--mcp") {
                patch.insert("mcp".into(), json!({ "listen": listen(&raw)? }));
                touched = true;
            } else if has("--mcp") {
                return Err(SetError::MissingValue("--mcp".into()));
            }
            if !touched {
                return Err(SetError::MissingTarget);
            }
        }
        "user" => {
            if has("--clear") {
                patch.insert("username".into(), json!(""));
                patch.insert("password".into(), json!(""));
            } else {
                if let Some(name) = value_of("--name") {
                    patch.insert("username".into(), json!(name));
                }
                if let Some(password) = value_of("--password") {
                    patch.insert("password".into(), json!(password));
                }
                if patch.is_empty() {
                    return Err(SetError::NothingToChange);
                }
            }
        }
        "language" => {
            let raw = positional(args, 0)
                .ok_or(SetError::MissingValue("set language".into()))?;
            if !matches!(raw.as_str(), "auto" | "zh-Hans" | "en-US") {
                return Err(SetError::BadValue {
                    what: crate::i18n::set_field_label(crate::i18n::current(), "language"),
                    value: raw,
                    want: "auto | zh-Hans | en-US",
                });
            }
            patch.insert("language".into(), json!(raw));
        }
        "login-interval" => {
            let raw = positional(args, 0)
                .ok_or(SetError::MissingValue("set login-interval".into()))?;
            let seconds: u64 = raw.parse().map_err(|_| SetError::BadValue {
                what: crate::i18n::set_field_label(crate::i18n::current(), "login-interval"),
                value: raw.clone(),
                want: crate::i18n::set_expected(crate::i18n::current(), "login-interval"),
            })?;
            patch.insert("loginIntervalSeconds".into(), json!(seconds));
        }
        "log-retain" => {
            let raw =
                positional(args, 0).ok_or(SetError::MissingValue("set log-retain".into()))?;
            let days: u64 = raw.parse().map_err(|_| SetError::BadValue {
                what: crate::i18n::set_field_label(crate::i18n::current(), "log-retain"),
                value: raw.clone(),
                want: crate::i18n::set_expected(crate::i18n::current(), "log-retain"),
            })?;
            patch.insert("logRetainDays".into(), json!(days));
        }
        // Host-wide lock (design doc §7.6.6, end)
        "host-lock" => {
            let raw =
                positional(args, 0).ok_or(SetError::MissingValue("set host-lock".into()))?;
            let on = match raw.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "yes" => true,
                "off" | "false" | "0" | "no" => false,
                _ => {
                    return Err(SetError::BadValue {
                        what: crate::i18n::set_field_label(crate::i18n::current(), "host-lock"),
                        value: raw,
                        want: crate::i18n::set_expected(crate::i18n::current(), "host-lock"),
                    })
                }
            };
            patch.insert("hostLock".into(), json!(on));
        }
        "roots" => {
            let action = positional(args, 0).unwrap_or_default();
            return match action.as_str() {
                "" | "list" => Ok(Intent::RootsList),
                "add" | "rm" | "remove" | "set" => {
                    let paths: Vec<String> = args
                        .iter()
                        .skip(1)
                        .map(|value| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                        .collect();
                    if paths.is_empty() {
                        return Err(SetError::NeedPath(action));
                    }
                    let key = match action.as_str() {
                        "add" => "rootsAdd",
                        "set" => "allowedRoots",
                        _ => "rootsRemove",
                    };
                    patch.insert(key.into(), json!(paths));
                    Ok(Intent::Patch(Value::Object(patch)))
                }
                other => Err(SetError::UnknownAction {
                    category: "roots".into(),
                    action: other.to_string(),
                }),
            };
        }
        "mcp" => {
            let raw = positional(args, 0).ok_or(SetError::MissingValue("set mcp".into()))?;
            let enabled = match raw.to_ascii_lowercase().as_str() {
                "on" | "enable" | "true" | "1" | "yes" => true,
                "off" | "disable" | "false" | "0" | "no" => false,
                _ => {
                    return Err(SetError::BadValue {
                        what: crate::i18n::set_field_label(crate::i18n::current(), "mcp"),
                        value: raw,
                        want: crate::i18n::set_expected(crate::i18n::current(), "mcp"),
                    })
                }
            };
            patch.insert("mcp".into(), json!({ "enabled": enabled }));
        }
        "all" => return Ok(Intent::Overview),
        other => return Err(SetError::UnknownCategory(other.to_string())),
    }

    Ok(Intent::Patch(Value::Object(patch)))
}

fn positional(args: &[String], index: usize) -> Option<String> {
    args.iter()
        .filter(|token| !token.starts_with("--"))
        .nth(index)
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

fn port(raw: &str, flag: &str) -> Result<u16, SetError> {
    raw.parse().map_err(|_| SetError::BadValue {
        what: format!(
            "{} {}",
            crate::i18n::set_field_label(crate::i18n::current(), "port"),
            flag
        ),
        value: raw.to_string(),
        want: crate::i18n::set_expected(crate::i18n::current(), "port"),
    })
}

fn listen(raw: &str) -> Result<String, SetError> {
    if raw != "lan" && raw != "wan" {
        return Err(SetError::BadValue {
            what: crate::i18n::set_field_label(crate::i18n::current(), "listen"),
            value: raw.to_string(),
            want: crate::i18n::set_expected(crate::i18n::current(), "listen"),
        });
    }
    Ok(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(input: &str) -> Result<Intent, SetError> {
        parse(&tokenize(input))
    }

    fn patch_of(input: &str) -> Value {
        match parse_str(input) {
            Ok(Intent::Patch(value)) => value,
            other => panic!("{input} should be a patch, got {other:?}"),
        }
    }

    #[test]
    fn bare_set_is_the_overview() {
        assert_eq!(parse_str("").unwrap(), Intent::Overview);
        assert_eq!(parse_str("all").unwrap(), Intent::Overview);
        assert_eq!(parse_str("all --json").unwrap(), Intent::Overview);
    }

    #[test]
    fn ports_are_two_independent_sides() {
        assert_eq!(patch_of("port --webui 65251"), json!({ "port": 65251 }));
        assert_eq!(patch_of("port --mcp 3994"), json!({ "mcp": { "port": 3994 } }));
        assert_eq!(
            patch_of("port --webui 1 --mcp 2"),
            json!({ "port": 1, "mcp": { "port": 2 } })
        );
        // Both equals-sign forms
        assert_eq!(patch_of("port --mcp=3994"), json!({ "mcp": { "port": 3994 } }));
        assert_eq!(parse_str("port"), Err(SetError::MissingTarget));
        assert_eq!(
            parse_str("port --webui"),
            Err(SetError::MissingValue("--webui".into()))
        );
        assert!(matches!(parse_str("port --webui 99999"), Err(SetError::BadValue { .. })));
    }

    #[test]
    fn listen_user_language_and_numbers() {
        assert_eq!(patch_of("listen --mcp wan"), json!({ "mcp": { "listen": "wan" } }));
        assert_eq!(
            patch_of("user --name alice --password s3cret"),
            json!({ "username": "alice", "password": "s3cret" })
        );
        assert_eq!(
            patch_of("user --clear"),
            json!({ "username": "", "password": "" })
        );
        assert_eq!(parse_str("user"), Err(SetError::NothingToChange));
        assert_eq!(patch_of("language zh-Hans"), json!({ "language": "zh-Hans" }));
        assert!(matches!(parse_str("language klingon"), Err(SetError::BadValue { .. })));
        assert_eq!(
            patch_of("login-interval 30"),
            json!({ "loginIntervalSeconds": 30 })
        );
        assert_eq!(patch_of("log-retain 9"), json!({ "logRetainDays": 9 }));
    }

    /// Host-wide lock (design doc §7.6.6, end): accepts both `on|off` and `true|false`.
    #[test]
    fn host_lock_accepts_on_off_and_booleans() {
        assert_eq!(patch_of("host-lock on"), json!({ "hostLock": true }));
        assert_eq!(patch_of("host-lock true"), json!({ "hostLock": true }));
        assert_eq!(patch_of("host-lock off"), json!({ "hostLock": false }));
        assert_eq!(patch_of("host-lock FALSE"), json!({ "hostLock": false }));
        // Missing or junk values must error clearly, never guess
        assert!(matches!(
            parse_str("host-lock"),
            Err(SetError::MissingValue(_))
        ));
        assert!(matches!(
            parse_str("host-lock maybe"),
            Err(SetError::BadValue { .. })
        ));
    }

    #[test]
    fn roots_supports_add_remove_and_list() {
        assert_eq!(parse_str("roots").unwrap(), Intent::RootsList);
        assert_eq!(parse_str("roots list").unwrap(), Intent::RootsList);
        assert_eq!(
            patch_of("roots add F:\\CODEX"),
            json!({ "rootsAdd": ["F:\\CODEX"] })
        );
        assert_eq!(
            patch_of("roots rm D:\\old"),
            json!({ "rootsRemove": ["D:\\old"] })
        );
        assert_eq!(
            patch_of("roots set C:\\a C:\\b"),
            json!({ "allowedRoots": ["C:\\a", "C:\\b"] })
        );
        assert_eq!(
            parse_str("roots add"),
            Err(SetError::NeedPath("add".into()))
        );
        // Paths with spaces: wrap them in quotes
        assert_eq!(
            patch_of("roots add \"F:\\my work\\src\""),
            json!({ "rootsAdd": ["F:\\my work\\src"] })
        );
    }

    #[test]
    fn mcp_switch_and_unknown_category() {
        assert_eq!(patch_of("mcp off"), json!({ "mcp": { "enabled": false } }));
        assert_eq!(patch_of("mcp on"), json!({ "mcp": { "enabled": true } }));
        assert!(matches!(
            parse_str("mcp maybe"),
            Err(SetError::BadValue { .. })
        ));
        assert_eq!(
            parse_str("nope"),
            Err(SetError::UnknownCategory("nope".into()))
        );
    }

    #[test]
    fn tokenize_handles_quotes() {
        assert_eq!(tokenize("a b  c"), vec!["a", "b", "c"]);
        assert_eq!(tokenize("a \"b c\" d"), vec!["a", "b c", "d"]);
        assert_eq!(tokenize("'x y'"), vec!["x y"]);
        assert!(tokenize("   ").is_empty());
    }
}

//! Runtime language selection for all user-facing text.

use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Result, bail};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Language {
    #[value(name = "en", alias = "english")]
    English,
    #[value(name = "de", alias = "deutsch", alias = "german")]
    German,
}

static LANGUAGE: AtomicU8 = AtomicU8::new(Language::English as u8);

pub fn configure(option: Option<Language>) -> Result<Language> {
    let language = match option {
        Some(language) => language,
        None => match std::env::var("BRIDGET_LANG") {
            Ok(value) => parse(&value)?,
            Err(std::env::VarError::NotPresent) => Language::English,
            Err(e) => return Err(e.into()),
        },
    };
    LANGUAGE.store(language as u8, Ordering::Relaxed);
    Ok(language)
}

pub fn parse(value: &str) -> Result<Language> {
    match value.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => Ok(Language::English),
        "de" | "deutsch" | "german" => Ok(Language::German),
        _ => bail!(
            "unsupported language {value:?}; use en or de / nicht unterstützte Sprache {value:?}; en oder de verwenden"
        ),
    }
}

/// Nimmt `--lang X` / `--lang=X` aus den Argumenten (nur vor `--`, Programmargumente bleiben
/// unberührt). clap würde sonst einen Unterbefehl danach als Konsolennamen lesen
/// (`args_conflicts_with_subcommands`), z.B. `bridget --lang de pair`.
pub fn take(args: Vec<std::ffi::OsString>) -> Result<(Vec<std::ffi::OsString>, Option<Language>)> {
    let mut requested = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            rest.push(arg);
            rest.extend(iter);
            break;
        }
        if arg == "--lang" {
            let value = iter.next().unwrap_or_default();
            requested = Some(parse(&value.to_string_lossy())?);
        } else if let Some(value) = arg.to_str().and_then(|a| a.strip_prefix("--lang=")) {
            requested = Some(parse(value)?);
        } else {
            rest.push(arg);
        }
    }
    Ok((rest, requested))
}

pub fn german() -> bool {
    LANGUAGE.load(Ordering::Relaxed) == Language::German as u8
}

pub fn code() -> &'static str {
    if german() { "de" } else { "en" }
}

#[macro_export]
macro_rules! tr {
    ($english:expr, $german:expr) => {
        if $crate::locale::german() { $german } else { $english }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lang_is_taken_out_before_the_separator_only() {
        let os = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        let (rest, lang) = take(os(&["bridget", "--lang", "de", "pair", "claude"])).unwrap();
        assert_eq!(rest, os(&["bridget", "pair", "claude"]));
        assert_eq!(lang, Some(Language::German));
        let (rest, lang) = take(os(&["bridget", "A", "--", "tool", "--lang=de"])).unwrap();
        assert_eq!(rest, os(&["bridget", "A", "--", "tool", "--lang=de"]));
        assert_eq!(lang, None);
        assert!(take(os(&["bridget", "--lang=fr", "ls"])).is_err());
    }

    #[test]
    fn language_names_are_parsed() {
        assert_eq!(parse("en").unwrap(), Language::English);
        assert_eq!(parse("Deutsch").unwrap(), Language::German);
        assert!(parse("fr").is_err());
    }
}

use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u8)]
pub enum Language {
    #[default]
    English = 0,
    Chinese = 1,
}

static CURRENT_LANGUAGE: AtomicU8 = AtomicU8::new(if cfg!(test) { 1 } else { 0 });
static TRANSLATIONS: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();

pub fn set_current_language(language: Language) {
    CURRENT_LANGUAGE.store(language as u8, Ordering::Relaxed);
}

pub fn current_language() -> Language {
    match CURRENT_LANGUAGE.load(Ordering::Relaxed) {
        1 => Language::Chinese,
        _ => Language::English,
    }
}

pub fn text(english: &'static str, chinese: &'static str) -> &'static str {
    match current_language() {
        Language::English => english,
        Language::Chinese => chinese,
    }
}

fn pairs() -> &'static [(&'static str, &'static str)] {
    TRANSLATIONS.get_or_init(|| {
        let data = include_str!("../resources/i18n/en-US.json");
        let map: HashMap<&'static str, &'static str> =
            serde_json::from_str(data).expect("English translation catalog must be valid JSON");
        let mut pairs: Vec<_> = map.into_iter().map(|(zh, en)| (zh, en)).collect();
        pairs.sort_by_key(|(zh, _)| std::cmp::Reverse(zh.len()));
        pairs
    })
}

fn match_template(template: &str, value: &str) -> Option<Vec<String>> {
    let mut chunks = Vec::new();
    let mut holes = 0usize;
    let mut cursor = 0usize;
    while let Some(open_rel) = template[cursor..].find('{') {
        let open = cursor + open_rel;
        if template.as_bytes().get(open + 1) == Some(&b'{') {
            cursor = open + 2;
            continue;
        }
        let close = template[open..].find('}')? + open;
        chunks.push(&template[cursor..open]);
        holes += 1;
        cursor = close + 1;
    }
    if holes == 0 {
        return (template == value).then(Vec::new);
    }
    chunks.push(&template[cursor..]);
    let mut remaining = value;
    let mut captures = Vec::with_capacity(holes);
    remaining = remaining.strip_prefix(chunks[0])?;
    for index in 0..holes {
        let next = chunks[index + 1];
        if index + 1 == holes {
            if next.is_empty() {
                captures.push(remaining.to_owned());
                remaining = "";
            } else {
                let end = remaining.rfind(next)?;
                captures.push(remaining[..end].to_owned());
                remaining = &remaining[end + next.len()..];
            }
        } else if next.is_empty() {
            captures.push(String::new());
        } else {
            let end = remaining.find(next)?;
            captures.push(remaining[..end].to_owned());
            remaining = &remaining[end + next.len()..];
        }
    }
    remaining.is_empty().then_some(captures)
}

fn render_template(template: &str, captures: &[String]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0usize;
    let mut capture = 0usize;
    while let Some(open_rel) = template[cursor..].find('{') {
        let open = cursor + open_rel;
        if template.as_bytes().get(open + 1) == Some(&b'{') {
            output.push_str(&template[cursor..open + 2]);
            cursor = open + 2;
            continue;
        }
        let Some(close_rel) = template[open..].find('}') else {
            break;
        };
        let close = open + close_rel;
        output.push_str(&template[cursor..open]);
        if let Some(value) = captures.get(capture) {
            output.push_str(value);
        }
        capture += 1;
        cursor = close + 1;
    }
    output.push_str(&template[cursor..]);
    output
}

pub fn localize_dynamic(language: Language, value: &str) -> String {
    for &(chinese, english) in pairs() {
        let (from, to) = match language {
            Language::English => (chinese, english),
            Language::Chinese => (english, chinese),
        };
        if value == from {
            return to.to_owned();
        }
        if let Some(captures) = match_template(from, value) {
            return render_template(to, &captures);
        }
    }
    value.to_owned()
}

fn preference_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME")
        .map(PathBuf::from)?
        .join("Library/Application Support");
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from)?;
    #[cfg(target_os = "linux")]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".config"))
        })?;
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    let base = env::var_os("HOME").map(PathBuf::from)?.join(".config");
    Some(base.join("PhotoArchiveManager").join("ui-language"))
}

pub fn load_preference() -> Language {
    preference_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|value| {
            if value.trim() == "zh-CN" {
                Language::Chinese
            } else {
                Language::English
            }
        })
        .unwrap_or_default()
}

pub fn save_preference(language: Language) -> std::io::Result<()> {
    let Some(path) = preference_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "configuration folder unavailable",
        ));
    };
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration path has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    fs::write(
        path,
        match language {
            Language::English => "en-US",
            Language::Chinese => "zh-CN",
        },
    )
}

#[macro_export]
macro_rules! tr_format {
    ($english:literal, $chinese:literal $(, $argument:expr)* $(,)?) => {{
        match $crate::i18n::current_language() {
            $crate::i18n::Language::English => format!($english $(, $argument)*),
            $crate::i18n::Language::Chinese => format!($chinese $(, $argument)*),
        }
    }};
}

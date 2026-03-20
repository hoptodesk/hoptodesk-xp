
use serde_json::{json, value::Value};
use std::ops::Deref;
use std::sync::Mutex;

mod ar;
mod be;
mod bg;
mod ca;
mod cn;
mod cs;
mod da;
mod de;
mod el;
mod en;
mod eo;
mod es;
mod et;
mod eu;
mod fa;
mod fr;
mod he;
mod hr;
mod hu;
mod id;
mod it;
mod ja;
mod ko;
mod kz;
mod lt;
mod lv;
mod nb;
mod nl;
mod pl;
mod ptbr;
mod ro;
mod ru;
mod sk;
mod sl;
mod sq;
mod sr;
mod sv;
mod th;
mod tr;
mod tw;
mod uk;
mod vi;
mod fi;

lazy_static::lazy_static! {
    pub static ref LANGS: Value =
        json!(vec![
            ("en", "English"),
            ("fr", "Français"),
            ("es", "Español"),
            ("it", "Italiano"),
            ("de", "Deutsch"),
            ("nl", "Nederlands"),
            ("pt", "Português (Brazil)"),
            ("ca", "Català"),
            ("eo", "Esperanto"),
            ("eu", "Euskara"),
            ("cs", "Čeština"),
            ("hu", "Magyar"),
            ("da", "Dansk"),
            ("nb", "Norsk bokmål"),
            ("sv", "Svenska"),
            ("fi", "Suomi"),
            ("pl", "Polski"),
            ("lt", "Lietuvių"),
            ("lv", "Latviešu"),
            ("et", "Eesti keel"),
            ("sr", "Srpski"),
            ("hr", "Hrvatski"),
            ("sq", "Shqip"),
            ("sk", "Slovenčina"),
            ("sl", "Slovenščina"),
            ("ro", "Română"),
            ("bg", "български"),
            ("be", "Беларуская"),
            ("el", "Ελληνικά"),
            ("tr", "Türkçe"),
            ("ru", "Русский"),
            ("uk", "Українська"),
            ("kz", "Қазақ"),
            ("ar", "العربية"),
            ("he", "עברית"),
            ("fa", "فارسی"),
            ("id", "Indonesia"),
            ("vi", "Tiếng Việt"),
            ("th", "ไทย"),
        ]);

    static ref CURRENT_LANG: Mutex<String> = Mutex::new(String::new());
}

pub fn set_lang(lang: &str) {
    if let Ok(mut l) = CURRENT_LANG.lock() {
        *l = lang.to_lowercase();
    }
}

fn get_system_locale() -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultLangID() -> u16;
    }
    let lang_id = unsafe { GetUserDefaultLangID() };
    let primary = lang_id & 0x3FF;
    let sub = (lang_id >> 10) & 0x3F;
    match primary {
        0x04 => {
            if sub == 0x01 { "zh-cn" } else { "zh-tw" }
        }
        0x09 => "en",
        0x0C => "fr",
        0x0A => "es",
        0x10 => "it",
        0x07 => "de",
        0x13 => "nl",
        0x16 => "ptbr",
        0x03 => "ca",
        0x05 => "cs",
        0x0E => "hu",
        0x06 => "da",
        0x14 => "nb",
        0x1D => "sv",
        0x0B => "fi",
        0x15 => "pl",
        0x27 => "lt",
        0x26 => "lv",
        0x25 => "et",
        0x1A => "hr",
        0x1C => "sq",
        0x1B => "sk",
        0x24 => "sl",
        0x18 => "ro",
        0x02 => "bg",
        0x23 => "be",
        0x08 => "el",
        0x1F => "tr",
        0x19 => "ru",
        0x22 => "uk",
        0x3F => "kz",
        0x01 => "ar",
        0x0D => "he",
        0x29 => "fa",
        0x21 => "id",
        0x12 => "ko",
        0x11 => "ja",
        0x2A => "vi",
        0x1E => "th",
        _ => "en",
    }.to_string()
}

pub fn translate(name: String) -> String {
    let lang = {
        let l = CURRENT_LANG.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_empty() {
            get_system_locale()
        } else {
            l.clone()
        }
    };
    translate_locale(name, &lang)
}

pub fn translate_locale(name: String, lang: &str) -> String {
    let m = match lang {
        "fr" => fr::T.deref(),
        "zh-cn" => cn::T.deref(),
        "it" => it::T.deref(),
        "zh-tw" => tw::T.deref(),
        "de" => de::T.deref(),
        "nb" => nb::T.deref(),
        "nl" => nl::T.deref(),
        "es" => es::T.deref(),
        "et" => et::T.deref(),
        "eu" => eu::T.deref(),
        "hu" => hu::T.deref(),
        "ru" => ru::T.deref(),
        "eo" => eo::T.deref(),
        "id" => id::T.deref(),
        "ptbr" | "br" | "pt" => ptbr::T.deref(),
        "tr" => tr::T.deref(),
        "cs" => cs::T.deref(),
        "da" => da::T.deref(),
        "sk" => sk::T.deref(),
        "vi" => vi::T.deref(),
        "pl" => pl::T.deref(),
        "ja" => ja::T.deref(),
        "ko" => ko::T.deref(),
        "kz" => kz::T.deref(),
        "uk" => uk::T.deref(),
        "fa" => fa::T.deref(),
        "fi" => fi::T.deref(),
        "ca" => ca::T.deref(),
        "el" => el::T.deref(),
        "sv" => sv::T.deref(),
        "sq" => sq::T.deref(),
        "sr" => sr::T.deref(),
        "th" => th::T.deref(),
        "sl" => sl::T.deref(),
        "ro" => ro::T.deref(),
        "lt" => lt::T.deref(),
        "lv" => lv::T.deref(),
        "ar" => ar::T.deref(),
        "bg" => bg::T.deref(),
        "be" => be::T.deref(),
        "he" => he::T.deref(),
        "hr" => hr::T.deref(),
        _ => en::T.deref(),
    };
    if let Some(v) = m.get(&name as &str) {
        if v.is_empty() {
            if lang != "en" {
                if let Some(v) = en::T.get(&name as &str) {
                    return v.to_string();
                }
            }
        } else {
            return v.to_string();
        }
    }
    name
}

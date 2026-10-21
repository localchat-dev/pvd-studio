use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "en")]
    English,
    #[default]
    #[serde(rename = "zh-CN", other)]
    Chinese,
}

impl Language {
    pub fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }

    pub fn label(self) -> &'static str {
        self.text("English", "简体中文")
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" | "en-US" | "en-us" => Some(Self::English),
            "zh" | "zh-CN" | "zh-cn" => Some(Self::Chinese),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    english: String,
    chinese: String,
}

impl Message {
    pub fn new(english: impl Into<String>, chinese: impl Into<String>) -> Self {
        Self {
            english: english.into(),
            chinese: chinese.into(),
        }
    }

    pub fn text(&self, language: Language) -> &str {
        language.text(&self.english, &self.chinese)
    }

    pub fn detail(english: &str, chinese: &str, detail: impl std::fmt::Display) -> Self {
        Self::new(
            format!("{english}: {detail}"),
            format!("{chinese}：{detail}"),
        )
    }

    pub fn context(self, english: &str, chinese: &str) -> Self {
        Self::new(
            format!("{english}: {}", self.english),
            format!("{chinese}：{}", self.chinese),
        )
    }
}

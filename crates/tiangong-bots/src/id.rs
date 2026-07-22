//! Bot 实例 ID 的统一校验。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

/// 经过校验、可安全用于 Bot 运行目录的实例 ID。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BotId(String);

impl BotId {
    /// 返回 ID 的只读字符串表示。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for BotId {
    type Error = InvalidBotId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate(value)?;
        Ok(Self(value.to_string()))
    }
}

impl TryFrom<String> for BotId {
    type Error = InvalidBotId;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate(&value)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for BotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(D::Error::custom)
    }
}

/// Bot ID 校验失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBotId {
    value: String,
    reason: &'static str,
}

impl fmt::Display for InvalidBotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Bot ID 非法 `{}`：{}（需为 1～64 位小写英文或数字，且首位为英文字母）",
            self.value, self.reason
        )
    }
}

impl std::error::Error for InvalidBotId {}

fn validate(value: &str) -> Result<(), InvalidBotId> {
    let invalid = |reason| InvalidBotId {
        value: value.to_string(),
        reason,
    };

    if value.is_empty() || value.len() > 64 {
        return Err(invalid("长度必须为 1～64 个字符"));
    }

    let mut bytes = value.bytes();
    if !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase()) {
        return Err(invalid("首位必须是小写英文字母"));
    }
    if !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()) {
        return Err(invalid("只能包含小写英文字母和数字"));
    }
    if is_windows_reserved_name(value) {
        return Err(invalid("不能使用 Windows 保留名称"));
    }
    Ok(())
}

fn is_windows_reserved_name(value: &str) -> bool {
    matches!(value, "con" | "prn" | "aux" | "nul")
        || matches!(
            value.strip_prefix("com"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
        || matches!(
            value.strip_prefix("lpt"),
            Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        for value in ["feishu", "feishu1", "bot2026", "a"] {
            assert!(BotId::try_from(value).is_ok(), "{value}");
        }
        assert!(BotId::try_from(format!("a{}", "1".repeat(63))).is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        for value in [
            "",
            "1feishu",
            "Feishu",
            "feishu-bot",
            "feishu.exe",
            "../feishu",
            "feishu bot",
            "con",
            "com1",
            "lpt9",
            "nul",
        ] {
            assert!(BotId::try_from(value).is_err(), "{value}");
        }
        assert!(BotId::try_from(format!("a{}", "1".repeat(64))).is_err());
    }

    #[test]
    fn serde_rejects_invalid_id() {
        let error = serde_json::from_str::<BotId>(r#""../feishu""#).unwrap_err();
        assert!(error.to_string().contains("Bot ID 非法"));
    }
}

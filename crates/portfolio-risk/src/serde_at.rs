//! `at_core` 的型別沒有 serde derive（那個 crate 刻意零外部依賴），所以
//! `#[serde(with = "...")]` 用的轉換寫在這裡。
//!
//! 一律轉成**十進位字串**，和 Binance 的 JSON 回應、`at-account-sync` 的費率欄位
//! 同一個慣例。不存內部整數的理由很實際：設定檔是人會打開來看、甚至手改的東西，
//! `"0.2"` 看得懂，`20000000` 看不懂；而 `Fixed` 的 `Display` 與 `FromStr`
//! 已經是精確的來回轉換（`fixed.rs` 的 `display_then_parse_round_trips` 有測）。

/// `Fixed` ⇄ 十進位字串。
pub mod fixed {
    use at_core::Fixed;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Fixed, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Fixed, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(D::Error::custom)
    }
}

/// `Option<Symbol>` ⇄ `Option<String>`。
///
/// 反序列化會走 [`at_core::Symbol::new`]，所以檔案裡的 `"btc/usdt"` 這種值會被
/// 拒絕而不是悄悄變成一個不存在的交易對。
pub mod symbol_opt {
    use at_core::Symbol;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Symbol>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(symbol) => serializer.serialize_some(symbol.as_str()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Symbol>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            Some(text) => Symbol::new(&text).map(Some).map_err(D::Error::custom),
            None => Ok(None),
        }
    }
}

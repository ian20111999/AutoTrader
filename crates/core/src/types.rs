use std::fmt;

/// 交易市場。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    /// Binance 現貨
    Spot,
    /// Binance U 本位永續合約
    UsdmPerp,
}

impl Market {
    pub fn label_zh(self) -> &'static str {
        match self {
            Market::Spot => "現貨",
            Market::UsdmPerp => "合約",
        }
    }
}

/// 買賣方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// 成交時是掛單（maker）還是吃單（taker），決定套用哪個手續費率。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Liquidity {
    Maker,
    Taker,
}

/// 執行模式。四種模式跑的是同一份策略程式，只差在成交從哪裡來。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunMode {
    /// 歷史資料回測
    Backtest,
    /// 即時行情、模擬成交
    Paper,
    /// Binance 測試網，真實下單但不是真錢
    Testnet,
    /// 實盤，真實資金
    Live,
}

impl RunMode {
    pub const ALL: [RunMode; 4] = [
        RunMode::Backtest,
        RunMode::Paper,
        RunMode::Testnet,
        RunMode::Live,
    ];

    pub fn label_zh(self) -> &'static str {
        match self {
            RunMode::Backtest => "回測",
            RunMode::Paper => "模擬交易",
            RunMode::Testnet => "測試網",
            RunMode::Live => "實盤",
        }
    }

    /// 這個模式會不會動用真實資金。
    pub fn uses_real_money(self) -> bool {
        matches!(self, RunMode::Live)
    }

    /// 這個模式會不會把訂單送到交易所。
    pub fn sends_orders(self) -> bool {
        matches!(self, RunMode::Testnet | RunMode::Live)
    }
}

/// 交易對代號，例如 `BTCUSDT`。只允許大寫英文字母與數字。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolError {
    Empty,
    TooLong,
    InvalidChar(char),
}

impl fmt::Display for SymbolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SymbolError::Empty => write!(f, "交易對代號不能是空的"),
            SymbolError::TooLong => write!(f, "交易對代號太長（上限 20 字元）"),
            SymbolError::InvalidChar(c) => write!(f, "交易對代號含有不允許的字元：{c:?}"),
        }
    }
}

impl std::error::Error for SymbolError {}

impl Symbol {
    /// 建立交易對代號；小寫會自動轉成大寫。
    pub fn new(s: &str) -> Result<Symbol, SymbolError> {
        let s = s.trim().to_ascii_uppercase();
        if s.is_empty() {
            return Err(SymbolError::Empty);
        }
        if s.len() > 20 {
            return Err(SymbolError::TooLong);
        }
        if let Some(c) = s.chars().find(|c| !c.is_ascii_alphanumeric()) {
            return Err(SymbolError::InvalidChar(c));
        }
        Ok(Symbol(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_normalizes_case() {
        assert_eq!(Symbol::new(" btcusdt ").unwrap().as_str(), "BTCUSDT");
    }

    #[test]
    fn symbol_rejects_bad_input() {
        assert_eq!(Symbol::new(""), Err(SymbolError::Empty));
        assert_eq!(Symbol::new("BTC/USDT"), Err(SymbolError::InvalidChar('/')));
        assert_eq!(Symbol::new(&"A".repeat(21)), Err(SymbolError::TooLong));
    }

    #[test]
    fn only_live_uses_real_money() {
        let real: Vec<_> = RunMode::ALL
            .iter()
            .filter(|m| m.uses_real_money())
            .collect();
        assert_eq!(real, vec![&RunMode::Live]);
    }

    #[test]
    fn backtest_and_paper_never_send_orders() {
        assert!(!RunMode::Backtest.sends_orders());
        assert!(!RunMode::Paper.sends_orders());
        assert!(RunMode::Testnet.sends_orders());
        assert!(RunMode::Live.sends_orders());
    }

    #[test]
    fn side_opposite() {
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert_eq!(Side::Sell.opposite(), Side::Buy);
    }
}

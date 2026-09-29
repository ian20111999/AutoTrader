//! 定點數：用整數表示價格、數量與金額。
//!
//! 為什麼不用 `f64`？二進位浮點數無法精確表示 0.1，
//! `0.1 + 0.2` 會得到 `0.30000000000000004`。交易所的價格、數量
//! 都是十進位字串（例如 `"63880.10"`），下單時必須剛好對齊價格跳動與
//! 數量級距，差一點點就會被交易所拒單，所以引擎內部一律用整數。
//!
//! 做法：把數字乘上 10^8 存成 `i64`。Binance 的價格與數量最多 8 位小數，
//! 所以 `63880.10` 存成 `6_388_010_000_000`，完全精確。
//!
//! 限制：`i64` 最大約 9.2×10^18，除以 10^8 後可表示到約 922 億。
//! 超過時 `checked_*` 系列會回傳 `None`，不會默默算錯。

use std::fmt;
use std::str::FromStr;

/// 8 位小數的定點數。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Fixed(i64);

/// 解析字串失敗的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseFixedError {
    Empty,
    InvalidChar(char),
    TooManyDecimals,
    Overflow,
}

impl fmt::Display for ParseFixedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseFixedError::Empty => write!(f, "數字是空的"),
            ParseFixedError::InvalidChar(c) => write!(f, "數字含有不允許的字元：{c:?}"),
            ParseFixedError::TooManyDecimals => write!(f, "小數超過 8 位"),
            ParseFixedError::Overflow => write!(f, "數字太大，超出可表示範圍"),
        }
    }
}

impl std::error::Error for ParseFixedError {}

impl Fixed {
    /// 小數位數。
    pub const DECIMALS: u32 = 8;
    /// 1.0 對應的內部整數。
    pub const SCALE: i64 = 100_000_000;
    pub const ZERO: Fixed = Fixed(0);
    pub const ONE: Fixed = Fixed(Self::SCALE);

    /// 直接用內部整數建立（`from_raw(150_000_000)` 就是 1.5）。
    pub const fn from_raw(raw: i64) -> Fixed {
        Fixed(raw)
    }

    /// 內部整數。
    pub const fn raw(self) -> i64 {
        self.0
    }

    /// 由整數建立，例如 `from_int(3)` 就是 3.0。
    pub fn from_int(n: i64) -> Option<Fixed> {
        n.checked_mul(Self::SCALE).map(Fixed)
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn abs(self) -> Fixed {
        Fixed(self.0.abs())
    }

    pub fn checked_add(self, rhs: Fixed) -> Option<Fixed> {
        self.0.checked_add(rhs.0).map(Fixed)
    }

    pub fn checked_sub(self, rhs: Fixed) -> Option<Fixed> {
        self.0.checked_sub(rhs.0).map(Fixed)
    }

    /// 相乘（例如 價格 × 數量 = 金額）。
    ///
    /// 兩個 10^8 倍的整數相乘會變成 10^16 倍，所以要再除以 10^8。
    /// 中間用 `i128` 避免溢位；第 9 位小數四捨五入（0.5 遠離零）。
    pub fn checked_mul(self, rhs: Fixed) -> Option<Fixed> {
        let wide = self.0 as i128 * rhs.0 as i128;
        let scale = Self::SCALE as i128;
        let q = wide / scale;
        let r = wide % scale;
        let rounded = if r.abs() * 2 >= scale {
            q + wide.signum()
        } else {
            q
        };
        i64::try_from(rounded).ok().map(Fixed)
    }

    /// 相除（例如 金額 ÷ 價格 = 數量）。
    ///
    /// 被除數先放大 10^8，商才會是 10^8 倍的定點數。
    /// 中間用 `i128` 避免溢位；第 9 位小數四捨五入（0.5 遠離零）。
    /// 除以 0 回傳 `None`，不會 panic。
    pub fn checked_div(self, rhs: Fixed) -> Option<Fixed> {
        if rhs.0 == 0 {
            return None;
        }
        let wide = self.0 as i128 * Self::SCALE as i128;
        let divisor = rhs.0 as i128;
        let q = wide / divisor;
        let r = wide % divisor;
        let rounded = if r.abs() * 2 >= divisor.abs() {
            q + wide.signum() * divisor.signum()
        } else {
            q
        };
        i64::try_from(rounded).ok().map(Fixed)
    }

    /// 往下取整到 `step` 的倍數（往負無限大方向）。
    ///
    /// 例：`step = 0.01` 時 `1.239 → 1.23`、`-1.231 → -1.24`。
    /// `step` 必須大於 0，否則回傳 `None`。
    pub fn floor_to_step(self, step: Fixed) -> Option<Fixed> {
        if step.0 <= 0 {
            return None;
        }
        let q = self.0.div_euclid(step.0);
        q.checked_mul(step.0).map(Fixed)
    }

    /// 往上取整到 `step` 的倍數（往正無限大方向）。
    ///
    /// 例：`step = 0.01` 時 `1.231 → 1.24`。`step` 必須大於 0。
    pub fn ceil_to_step(self, step: Fixed) -> Option<Fixed> {
        let floor = self.floor_to_step(step)?;
        if floor == self {
            Some(floor)
        } else {
            floor.checked_add(step)
        }
    }

    /// 是否剛好是 `step` 的倍數。`step` ≤ 0 時一律回傳 `false`。
    pub fn is_multiple_of(self, step: Fixed) -> bool {
        step.0 > 0 && self.0 % step.0 == 0
    }

    /// 轉成 `f64`，只用在統計與畫圖，不可用在下單計算。
    pub fn to_f64(self) -> f64 {
        self.0 as f64 / Self::SCALE as f64
    }
}

impl FromStr for Fixed {
    type Err = ParseFixedError;

    /// 解析十進位字串，例如 `"63880.10"`、`"-0.5"`、`"0.00100000"`。
    /// 小數超過 8 位會回傳錯誤，而不是默默捨去。
    fn from_str(s: &str) -> Result<Fixed, ParseFixedError> {
        let s = s.trim();
        let (neg, body) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        if body.is_empty() {
            return Err(ParseFixedError::Empty);
        }
        let (int_part, frac_part) = match body.split_once('.') {
            Some((i, f)) => (i, f),
            None => (body, ""),
        };
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(ParseFixedError::Empty);
        }
        if let Some(c) = int_part
            .chars()
            .chain(frac_part.chars())
            .find(|c| !c.is_ascii_digit())
        {
            return Err(ParseFixedError::InvalidChar(c));
        }
        if frac_part.len() > Self::DECIMALS as usize {
            return Err(ParseFixedError::TooManyDecimals);
        }

        let mut raw: i64 = 0;
        for c in int_part.chars() {
            let d = c.to_digit(10).unwrap() as i64;
            raw = raw
                .checked_mul(10)
                .and_then(|v| v.checked_add(d))
                .ok_or(ParseFixedError::Overflow)?;
        }
        raw = raw
            .checked_mul(Self::SCALE)
            .ok_or(ParseFixedError::Overflow)?;

        // 小數部分補零到 8 位："10" → "10000000"
        let mut frac: i64 = 0;
        for i in 0..Self::DECIMALS as usize {
            let d = frac_part.as_bytes().get(i).map_or(0, |b| (b - b'0') as i64);
            frac = frac * 10 + d;
        }
        raw = raw.checked_add(frac).ok_or(ParseFixedError::Overflow)?;
        Ok(Fixed(if neg { -raw } else { raw }))
    }
}

impl fmt::Display for Fixed {
    /// 去掉小數尾端的 0：`63880.10000000` 顯示成 `63880.1`。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let scale = Self::SCALE as u64;
        let int = abs / scale;
        let frac = abs % scale;
        if frac == 0 {
            write!(f, "{sign}{int}")
        } else {
            let digits = format!("{frac:08}");
            write!(f, "{sign}{int}.{}", digits.trim_end_matches('0'))
        }
    }
}

impl fmt::Debug for Fixed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fixed({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(s: &str) -> Fixed {
        s.parse().unwrap()
    }

    #[test]
    fn f64_is_not_exact_but_fixed_is() {
        // 這就是不用 f64 的原因
        assert_ne!(0.1_f64 + 0.2_f64, 0.3_f64);
        assert_eq!(fx("0.1").checked_add(fx("0.2")), Some(fx("0.3")));
    }

    #[test]
    fn parses_binance_style_strings() {
        assert_eq!(fx("63880.10").raw(), 6_388_010_000_000);
        assert_eq!(fx("0.00100000").raw(), 100_000);
        assert_eq!(fx("-0.5").raw(), -50_000_000);
        assert_eq!(fx(".5"), fx("0.5"));
        assert_eq!(fx("7"), Fixed::from_int(7).unwrap());
    }

    #[test]
    fn rejects_bad_strings() {
        assert_eq!("".parse::<Fixed>(), Err(ParseFixedError::Empty));
        assert_eq!("-".parse::<Fixed>(), Err(ParseFixedError::Empty));
        assert_eq!(".".parse::<Fixed>(), Err(ParseFixedError::Empty));
        assert_eq!(
            "1,000".parse::<Fixed>(),
            Err(ParseFixedError::InvalidChar(','))
        );
        assert_eq!(
            "1e5".parse::<Fixed>(),
            Err(ParseFixedError::InvalidChar('e'))
        );
        assert_eq!(
            "0.123456789".parse::<Fixed>(),
            Err(ParseFixedError::TooManyDecimals)
        );
        assert_eq!(
            "99999999999999".parse::<Fixed>(),
            Err(ParseFixedError::Overflow)
        );
    }

    #[test]
    fn displays_without_trailing_zeros() {
        assert_eq!(fx("63880.10000000").to_string(), "63880.1");
        assert_eq!(fx("5").to_string(), "5");
        assert_eq!(fx("-0.05").to_string(), "-0.05");
        assert_eq!(fx("0.00000001").to_string(), "0.00000001");
    }

    #[test]
    fn display_then_parse_round_trips() {
        for s in ["0", "1", "-1", "0.00000001", "123.456", "-98765.4321"] {
            assert_eq!(fx(&fx(s).to_string()), fx(s));
        }
    }

    #[test]
    fn multiply_price_by_quantity() {
        // 0.0156 BTC × 63880.10 = 996.529560 USDT
        let notional = fx("0.0156").checked_mul(fx("63880.10")).unwrap();
        assert_eq!(notional, fx("996.52956"));
    }

    #[test]
    fn multiply_rounds_ninth_decimal_half_away_from_zero() {
        // 0.00000001 × 0.5 = 0.000000005 → 四捨五入到 0.00000001
        assert_eq!(
            fx("0.00000001").checked_mul(fx("0.5")),
            Some(fx("0.00000001"))
        );
        assert_eq!(
            fx("-0.00000001").checked_mul(fx("0.5")),
            Some(fx("-0.00000001"))
        );
        // 0.000000004 → 捨去成 0
        assert_eq!(fx("0.00000001").checked_mul(fx("0.4")), Some(Fixed::ZERO));
    }

    #[test]
    fn divide_amount_by_price_to_get_quantity() {
        // 1000 USDT ÷ 63880.10 = 0.01565433 BTC（第 9 位進位）
        assert_eq!(
            fx("1000").checked_div(fx("63880.10")),
            Some(fx("0.01565433"))
        );
        assert_eq!(fx("10000").checked_div(fx("100")), Some(fx("100")));
    }

    #[test]
    fn divide_rounds_ninth_decimal_half_away_from_zero() {
        // 1÷3 = 0.333…（捨）、2÷3 = 0.666…（進）；負號不影響進位方向
        assert_eq!(fx("1").checked_div(fx("3")), Some(fx("0.33333333")));
        assert_eq!(fx("2").checked_div(fx("3")), Some(fx("0.66666667")));
        assert_eq!(fx("-2").checked_div(fx("3")), Some(fx("-0.66666667")));
        assert_eq!(fx("2").checked_div(fx("-3")), Some(fx("-0.66666667")));
    }

    #[test]
    fn divide_by_zero_returns_none() {
        assert_eq!(fx("1").checked_div(Fixed::ZERO), None);
        assert_eq!(Fixed::ZERO.checked_div(Fixed::ZERO), None);
    }

    #[test]
    fn overflow_returns_none_instead_of_wrong_answer() {
        let big = Fixed::from_raw(i64::MAX);
        assert_eq!(big.checked_add(Fixed::from_raw(1)), None);
        assert_eq!(big.checked_mul(fx("2")), None);
        // 除以很小的數等於放大，同樣要回 None 而不是溢位
        assert_eq!(big.checked_div(fx("0.00000001")), None);
    }

    #[test]
    fn floor_to_step_rounds_down() {
        let step = fx("0.01");
        assert_eq!(fx("1.239").floor_to_step(step), Some(fx("1.23")));
        assert_eq!(fx("1.23").floor_to_step(step), Some(fx("1.23")));
        // 負數往負無限大方向，不是往 0
        assert_eq!(fx("-1.231").floor_to_step(step), Some(fx("-1.24")));
        assert_eq!(fx("0.00049").floor_to_step(fx("0.001")), Some(Fixed::ZERO));
    }

    #[test]
    fn ceil_to_step_rounds_up() {
        let step = fx("0.01");
        assert_eq!(fx("1.231").ceil_to_step(step), Some(fx("1.24")));
        assert_eq!(fx("1.24").ceil_to_step(step), Some(fx("1.24")));
        assert_eq!(fx("-1.239").ceil_to_step(step), Some(fx("-1.23")));
    }

    #[test]
    fn step_must_be_positive() {
        assert_eq!(fx("1.5").floor_to_step(Fixed::ZERO), None);
        assert_eq!(fx("1.5").ceil_to_step(fx("-0.1")), None);
        assert!(!fx("1.5").is_multiple_of(Fixed::ZERO));
    }

    #[test]
    fn is_multiple_of_step() {
        assert!(fx("0.03").is_multiple_of(fx("0.01")));
        assert!(!fx("0.035").is_multiple_of(fx("0.01")));
        assert!(Fixed::ZERO.is_multiple_of(fx("0.00001")));
    }

    #[test]
    fn ordering_matches_numeric_value() {
        assert!(fx("-1") < fx("0.5"));
        assert!(fx("0.5") < fx("0.50000001"));
    }
}

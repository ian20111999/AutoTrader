//! Session id 驗證：防止路徑穿越。
//!
//! `session_id` 之後會直接當資料夾名稱用（`<base_dir>/sessions/<id>/`），
//! 而且來源是外部（前端傳進 Tauri command）。ADR §9.6 點名這是這份設計
//! 唯一真實的安全漏洞來源：不驗證就組路徑，`../../etc/passwd` 這種 id
//! 就能讀寫 `base_dir` 以外的任意檔案。
//!
//! 防法是白名單：只允許 `[a-z0-9-]`，長度 1~100。這個字元集本身就不含
//! `.`、`/`、`\`，所以 `..`、絕對路徑、跳脫都會被擋在「不合法字元」這一條，
//! 不需要額外特判 `..`。

use std::fmt;

/// session id 不合法的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdError {
    /// 空字串。
    Empty,
    /// 超過 100 字元。
    TooLong,
    /// 含有白名單以外的字元（可能是路徑穿越嘗試）。
    InvalidChar(char),
}

impl fmt::Display for SessionIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionIdError::Empty => write!(f, "session id 不能是空的"),
            SessionIdError::TooLong => write!(f, "session id 太長（上限 100 字元）"),
            SessionIdError::InvalidChar(c) => {
                write!(f, "session id 含有不允許的字元：{c:?}")
            }
        }
    }
}

impl std::error::Error for SessionIdError {}

/// session id 的字元上限。
const MAX_LEN: usize = 100;

/// 驗證 session id 是不是「可以安全當資料夾名稱用」。
///
/// 每個會組出檔案系統路徑的公開函式，在組路徑**之前**都必須呼叫這個函式。
pub fn validate_session_id(id: &str) -> Result<(), SessionIdError> {
    if id.is_empty() {
        return Err(SessionIdError::Empty);
    }
    if id.len() > MAX_LEN {
        return Err(SessionIdError::TooLong);
    }
    if let Some(c) = id
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(SessionIdError::InvalidChar(c));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_well_formed_id() {
        assert_eq!(validate_session_id("paper-1790756100000-001"), Ok(()));
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(validate_session_id(""), Err(SessionIdError::Empty));
    }

    #[test]
    fn rejects_too_long() {
        let id = "a".repeat(101);
        assert_eq!(validate_session_id(&id), Err(SessionIdError::TooLong));
    }

    #[test]
    fn rejects_path_traversal_attempts() {
        // 這是這個模組存在的唯一理由：這些 id 絕不能被接受，
        // 不然組出來的路徑就會跳脫 base_dir。
        assert_eq!(
            validate_session_id(".."),
            Err(SessionIdError::InvalidChar('.'))
        );
        assert_eq!(
            validate_session_id("../../etc/passwd"),
            Err(SessionIdError::InvalidChar('.'))
        );
        assert_eq!(
            validate_session_id("a/../../b"),
            Err(SessionIdError::InvalidChar('/'))
        );
        assert_eq!(
            validate_session_id("..\\..\\windows"),
            Err(SessionIdError::InvalidChar('.'))
        );
        assert_eq!(
            validate_session_id("/etc/passwd"),
            Err(SessionIdError::InvalidChar('/'))
        );
        assert_eq!(
            validate_session_id("a\\b"),
            Err(SessionIdError::InvalidChar('\\'))
        );
    }

    #[test]
    fn rejects_uppercase_and_whitespace() {
        assert_eq!(
            validate_session_id("Paper-1"),
            Err(SessionIdError::InvalidChar('P'))
        );
        assert_eq!(
            validate_session_id("paper 1"),
            Err(SessionIdError::InvalidChar(' '))
        );
    }
}

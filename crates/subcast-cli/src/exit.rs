//! Semantic CLI exit codes and error classification.
use std::fmt;

pub const OK: i32 = 0;
pub const ERR: i32 = 1;
pub const USAGE: i32 = 2;
pub const NOT_FOUND: i32 = 3;
pub const MISSING_PREREQ: i32 = 4;
pub const NETWORK: i32 = 5;
pub const CANCELLED: i32 = 130;

#[derive(Debug)]
pub struct CliError {
    pub code: i32,
    pub message: String,
    pub kind: &'static str,
}

impl CliError {
    pub fn new(code: i32, kind: &'static str, msg: impl Into<String>) -> Self {
        Self {
            code,
            kind,
            message: msg.into(),
        }
    }

    pub fn usage(msg: impl Into<String>) -> Self {
        Self::new(USAGE, "usage", msg)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(NOT_FOUND, "not_found", msg)
    }

    pub fn missing(msg: impl Into<String>) -> Self {
        Self::new(MISSING_PREREQ, "missing_prerequisite", msg)
    }

    pub fn network(msg: impl Into<String>) -> Self {
        Self::new(NETWORK, "network", msg)
    }

    pub fn cancelled() -> Self {
        Self::new(CANCELLED, "cancelled", "operation cancelled")
    }

    pub fn from_anyhow(err: anyhow::Error) -> Self {
        let msg = format!("{err:#}");
        if msg.contains("HTTP")
            || msg.contains("error sending request")
            || msg.contains("timed out")
            || msg.contains("dns")
            || msg.contains("connect")
            || msg.contains("network")
            || msg.contains("网络")
            || msg.contains("超时")
        {
            return Self::network(msg);
        }
        if msg.contains("找不到")
            || msg.contains("不存在")
            || msg.contains("未找到")
            || msg.contains("没有匹配")
            || msg.contains("not found")
        {
            return Self::not_found(msg);
        }
        if msg.contains("API Key")
            || msg.contains("Key")
            || msg.contains("模型")
            || msg.contains("ffmpeg")
            || msg.contains("yt-dlp")
            || msg.contains("请先")
            || msg.contains("没有 YouTube 视频")
        {
            return Self::missing(msg);
        }
        Self::new(ERR, "error", msg)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<anyhow::Error> for CliError {
    fn from(value: anyhow::Error) -> Self {
        Self::from_anyhow(value)
    }
}

impl From<std::io::Error> for CliError {
    fn from(value: std::io::Error) -> Self {
        Self::from_anyhow(value.into())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ERR, "json", value.to_string())
    }
}

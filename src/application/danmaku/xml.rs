//! Structural validation of Bilibili/Dandanplay danmaku XML sidecars.

use std::fmt;

use quick_xml::{events::Event, reader::Reader};

pub const MAX_DANMAKU_XML_BYTES: usize = 50 * 1024 * 1024;

pub fn validate_danmaku_xml(bytes: &[u8]) -> Result<(), DanmakuXmlError> {
    if bytes.is_empty() {
        return Err(DanmakuXmlError::Empty);
    }
    if bytes.len() > MAX_DANMAKU_XML_BYTES {
        return Err(DanmakuXmlError::TooLarge);
    }

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut saw_danmaku = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                }
                if depth >= 1 && name == b"d" {
                    saw_danmaku = true;
                }
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                } else if name == b"d" {
                    saw_danmaku = true;
                }
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err(DanmakuXmlError::InvalidXml(
                        "unexpected closing element".to_owned(),
                    ));
                }
                depth -= 1;
            }
            Ok(Event::DocType(_)) => {
                return Err(DanmakuXmlError::InvalidXml(
                    "document type declarations are not allowed".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(DanmakuXmlError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    if !saw_root || depth != 0 {
        return Err(DanmakuXmlError::InvalidXml(
            "danmaku XML document is incomplete".to_owned(),
        ));
    }
    if !saw_danmaku {
        return Err(DanmakuXmlError::MissingDanmaku);
    }
    Ok(())
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|value| *value == b':').next().unwrap_or(name)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DanmakuXmlError {
    Empty,
    TooLarge,
    InvalidRoot,
    MissingDanmaku,
    InvalidXml(String),
}

impl fmt::Display for DanmakuXmlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("danmaku XML is empty"),
            Self::TooLarge => formatter.write_str("danmaku XML is too large"),
            Self::InvalidRoot => formatter.write_str("danmaku XML root must be <i>"),
            Self::MissingDanmaku => formatter.write_str("danmaku XML has no <d> entries"),
            Self::InvalidXml(message) => write!(formatter, "danmaku XML is invalid: {message}"),
        }
    }
}

impl std::error::Error for DanmakuXmlError {}

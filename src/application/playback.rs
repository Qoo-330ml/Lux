use std::fmt;

pub mod decision;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteRange {
    Full,
    Partial { start: u64, end: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeError {
    Invalid,
    Unsatisfiable,
}

impl fmt::Display for RangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Invalid => "invalid single byte range",
            Self::Unsatisfiable => "byte range is not satisfiable",
        })
    }
}

impl std::error::Error for RangeError {}

pub fn parse_single_range(header: Option<&str>, size: u64) -> Result<ByteRange, RangeError> {
    let Some(header) = header else {
        return Ok(ByteRange::Full);
    };
    let Some(specification) = header.trim().strip_prefix("bytes=") else {
        return Err(RangeError::Invalid);
    };
    if specification.contains(',') {
        return Err(RangeError::Invalid);
    }
    let Some((start, end)) = specification.split_once('-') else {
        return Err(RangeError::Invalid);
    };
    if start.is_empty() {
        let suffix_length = parse_u64(end)?;
        if suffix_length == 0 || size == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        let length = suffix_length.min(size);
        return Ok(ByteRange::Partial {
            start: size - length,
            end: size - 1,
        });
    }
    let start = parse_u64(start)?;
    if start >= size {
        return Err(RangeError::Unsatisfiable);
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        parse_u64(end)?.min(size - 1)
    };
    if start > end {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(ByteRange::Partial { start, end })
}

fn parse_u64(value: &str) -> Result<u64, RangeError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RangeError::Invalid);
    }
    value.parse().map_err(|_| RangeError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, RangeError, parse_single_range};

    #[test]
    fn parses_full_open_ended_and_bounded_ranges() {
        assert_eq!(parse_single_range(None, 10), Ok(ByteRange::Full));
        assert_eq!(
            parse_single_range(Some("bytes=2-5"), 10),
            Ok(ByteRange::Partial { start: 2, end: 5 })
        );
        assert_eq!(
            parse_single_range(Some("bytes=2-"), 10),
            Ok(ByteRange::Partial { start: 2, end: 9 })
        );
    }

    #[test]
    fn parses_suffix_and_clamps_end() {
        assert_eq!(
            parse_single_range(Some("bytes=-3"), 10),
            Ok(ByteRange::Partial { start: 7, end: 9 })
        );
        assert_eq!(
            parse_single_range(Some("bytes=8-99"), 10),
            Ok(ByteRange::Partial { start: 8, end: 9 })
        );
    }

    #[test]
    fn rejects_invalid_multiple_and_unsatisfiable_ranges() {
        assert_eq!(
            parse_single_range(Some("bytes=0-1,3-4"), 10),
            Err(RangeError::Invalid)
        );
        assert_eq!(
            parse_single_range(Some("items=0-1"), 10),
            Err(RangeError::Invalid)
        );
        assert_eq!(
            parse_single_range(Some("bytes=10-"), 10),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_single_range(Some("bytes=0-0"), 0),
            Err(RangeError::Unsatisfiable)
        );
    }
}

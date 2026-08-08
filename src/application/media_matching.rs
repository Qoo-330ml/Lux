use std::path::Path;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Movie,
    Series,
    Episode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMediaName {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub absolute_number: Option<u32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
}

pub fn parse_media_name(input: &str, kind: MediaKind) -> Option<ParsedMediaName> {
    let stem = Path::new(input).file_stem()?.to_str()?.trim();
    if stem.is_empty() {
        return None;
    }
    let normalized = normalize_separators(stem);
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    let production_year = words.iter().find_map(|word| parse_year(word));
    let (season, episode) = extract_sequence(&normalized);
    let edition_name = parse_edition_name(&words);
    let quality_label = parse_quality_label(&words);
    let title_input = if matches!(kind, MediaKind::Movie) {
        production_year
            .and_then(|year| {
                words
                    .iter()
                    .position(|word| *word == year.to_string())
                    .map(|index| words[..index].join(" "))
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| normalized.clone())
    } else {
        normalized.clone()
    };
    let mut title = clean_title_with_metadata(&title_input, production_year, season, episode);
    if title.is_empty() {
        return None;
    }
    if matches!(kind, MediaKind::Movie) {
        if let Some(edition) = edition_name.as_deref() {
            title = format!("{title} ({edition})");
        }
    }
    Some(ParsedMediaName {
        sort_title: title.to_lowercase(),
        title,
        production_year,
        season,
        episode,
        absolute_number: None,
        edition_name,
        quality_label,
    })
}

pub fn clean_title(value: &str) -> String {
    let normalized = normalize_separators(value);
    let production_year = normalized.split_whitespace().find_map(parse_year);
    let (season, episode) = extract_sequence(&normalized);
    clean_title_with_metadata(&normalized, production_year, season, episode)
}

pub fn title_candidates(title: &str) -> Vec<String> {
    let title = clean_title(title);
    if title.is_empty() {
        return Vec::new();
    }
    let mut candidates = vec![title.clone()];
    if let Some(stripped) = strip_trailing_season_number(&title) {
        candidates.push(stripped);
    }
    let cjk = title
        .chars()
        .filter(|character| is_cjk(*character) || character.is_ascii_digit())
        .collect::<String>();
    if !cjk.is_empty() && cjk != title {
        candidates.push(cjk);
    }
    let latin = title
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .collect::<String>()
        })
        .filter(|word| {
            word.chars()
                .any(|character| character.is_ascii_alphabetic())
        })
        .collect::<Vec<_>>()
        .join(" ");
    if !latin.is_empty() && latin != title {
        candidates.push(latin);
    }
    candidates.dedup();
    candidates
}

pub fn normalize_title(value: &str) -> String {
    value
        .chars()
        .filter_map(|character| {
            if character.is_alphanumeric() || is_cjk(character) {
                Some(character.to_ascii_lowercase())
            } else {
                None
            }
        })
        .collect()
}

fn normalize_separators(value: &str) -> String {
    let mut result = String::new();
    let characters = value.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        if let Some(year) = characters.get(index..index + 4).and_then(parse_year_chars)
            && (index == 0 || !characters[index - 1].is_ascii_digit())
            && (index + 4 == characters.len() || !characters[index + 4].is_ascii_digit())
        {
            if !result.ends_with(char::is_whitespace) && !result.is_empty() {
                result.push(' ');
            }
            result.push_str(&year.to_string());
            if index + 4 < characters.len() && !characters[index + 4].is_whitespace() {
                result.push(' ');
            }
            index += 4;
            continue;
        }
        let character = characters[index];
        result.push(match character {
            '.' | '_' | '-' | '(' | ')' | '[' | ']' | '{' | '}' | '（' | '）' | '【' | '】' => {
                ' '
            }
            _ => character,
        });
        index += 1;
    }
    result
}

fn parse_year(word: &str) -> Option<i32> {
    let characters = word.chars().collect::<Vec<_>>();
    parse_year_chars(&characters)
}

fn parse_year_chars(characters: &[char]) -> Option<i32> {
    if characters.len() != 4 || !characters.iter().all(char::is_ascii_digit) {
        return None;
    }
    let year = characters.iter().collect::<String>().parse::<i32>().ok()?;
    (1900..=2099).contains(&year).then_some(year)
}

fn extract_sequence(value: &str) -> (Option<u32>, Option<u32>) {
    let words = value
        .replace('第', " 第 ")
        .replace('季', " 季 ")
        .replace('集', " 集 ")
        .replace('话', " 话 ")
        .replace('話', " 話 ")
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for (index, word) in words.iter().enumerate() {
        if let Some((season, episode)) = parse_compact_sequence(word) {
            return (Some(season), Some(episode));
        }
        if word == "第" {
            let number = words.get(index + 1).and_then(|value| value.parse().ok());
            match words.get(index + 2).map(String::as_str) {
                Some("季") => {
                    let episode = (index + 3..words.len()).find_map(|position| {
                        (words.get(position).map(String::as_str) == Some("第"))
                            .then(|| words.get(position + 1).and_then(|value| value.parse().ok()))
                            .flatten()
                    });
                    return (number, episode);
                }
                Some("集" | "话" | "話") => return (Some(1), number),
                _ => {}
            }
        }
    }
    (None, None)
}

fn parse_compact_sequence(value: &str) -> Option<(u32, u32)> {
    let lowered = value.to_ascii_lowercase();
    let (season_start, separator) = if lowered.starts_with('s') {
        (1, 'e')
    } else {
        (0, 'x')
    };
    let separator_index = lowered[season_start..].find(separator)? + season_start;
    let season = lowered[season_start..separator_index].parse().ok()?;
    let episode = lowered[separator_index + 1..].parse().ok()?;
    Some((season, episode))
}

fn clean_title_with_metadata(
    value: &str,
    production_year: Option<i32>,
    season: Option<u32>,
    episode: Option<u32>,
) -> String {
    let words = value
        .replace('第', " 第 ")
        .replace('季', " 季 ")
        .replace('集', " 集 ")
        .replace('话', " 话 ")
        .replace('話', " 話 ")
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    let mut skip_chinese_number = false;
    for word in words {
        if parse_compact_sequence(&word).is_some() {
            continue;
        }
        if word == "第" || matches!(word.as_str(), "季" | "集" | "话" | "話") {
            skip_chinese_number = word == "第";
            continue;
        }
        if skip_chinese_number && word.chars().all(|character| character.is_ascii_digit()) {
            skip_chinese_number = false;
            continue;
        }
        skip_chinese_number = false;
        if production_year.is_some_and(|year| word == year.to_string())
            || is_technical_word(&word)
            || (season.is_some()
                && episode.is_some()
                && word == format_episode_marker(&season, &episode))
        {
            continue;
        }
        result.push(word);
    }
    result.join(" ")
}

fn format_episode_marker(season: &Option<u32>, episode: &Option<u32>) -> String {
    match (season, episode) {
        (Some(season), Some(episode)) => format!("S{season:02}E{episode:02}"),
        _ => String::new(),
    }
}

fn is_technical_word(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase().replace(' ', "");
    if is_multi_part_marker(&normalized) {
        return true;
    }
    if normalized == "h"
        || normalized
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return normalized == "h" || normalized == "264" || normalized == "265";
    }
    matches!(
        normalized.as_str(),
        "4k" | "8k"
            | "uhd"
            | "2160p"
            | "1080p"
            | "720p"
            | "576p"
            | "480p"
            | "hdr"
            | "hdr10"
            | "hdr10+"
            | "dv"
            | "dovi"
            | "sdr"
            | "web"
            | "dl"
            | "webdl"
            | "web-dl"
            | "webrip"
            | "bluray"
            | "blu-ray"
            | "bdrip"
            | "hdtv"
            | "remux"
            | "proper"
            | "repack"
            | "x264"
            | "x265"
            | "h264"
            | "h265"
            | "hevc"
            | "av1"
            | "8bit"
            | "10bit"
            | "aac"
            | "ac3"
            | "dd"
            | "ddp"
            | "dts"
            | "truehd"
            | "atmos"
            | "chdweb"
            | "chd"
            | "chs"
            | "cht"
            | "eng"
            | "sub"
            | "subs"
            | "中字"
            | "简繁"
            | "国语"
            | "粤语"
            | "director"
            | "directors"
            | "director's"
            | "extended"
            | "cut"
            | "unrated"
            | "theatrical"
            | "ultimate"
            | "final"
            | "special"
            | "remastered"
    )
}

fn is_multi_part_marker(value: &str) -> bool {
    ["cd", "disc", "disk", "part"].iter().any(|prefix| {
        value
            .strip_prefix(prefix)
            .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()))
    })
}

fn parse_edition_name(words: &[&str]) -> Option<String> {
    let lowered = words
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if lowered
        .windows(2)
        .any(|window| matches!(window, [first, second] if (first == "director" || first == "directors" || first == "director's") && *second == "cut"))
    {
        return Some("Director's Cut".to_owned());
    }
    if lowered
        .windows(2)
        .any(|window| matches!(window, [first, second] if *first == "extended" && *second == "cut"))
    {
        return Some("Extended Cut".to_owned());
    }
    [
        ("unrated", "Unrated"),
        ("theatrical", "Theatrical"),
        ("ultimate", "Ultimate"),
        ("final", "Final"),
        ("special", "Special"),
        ("remastered", "Remastered"),
    ]
    .iter()
    .find_map(|(token, label)| {
        lowered
            .iter()
            .any(|word| word == token)
            .then_some((*label).to_owned())
    })
}

fn parse_quality_label(words: &[&str]) -> Option<String> {
    let resolution = words
        .iter()
        .find_map(|word| match word.to_ascii_lowercase().as_str() {
            "4k" | "uhd" => Some("4K".to_owned()),
            "2160p" | "1080p" | "720p" | "576p" | "480p" => Some(word.to_ascii_lowercase()),
            _ => None,
        });
    let dynamic_range = words
        .iter()
        .find_map(|word| match word.to_ascii_lowercase().as_str() {
            "hdr" | "hdr10" | "hdr10+" => Some("HDR"),
            "sdr" => Some("SDR"),
            _ => None,
        });
    match (resolution, dynamic_range) {
        (Some(resolution), Some(dynamic_range)) => Some(format!("{resolution} {dynamic_range}")),
        (Some(resolution), None) => Some(resolution),
        (None, Some(dynamic_range)) => Some(dynamic_range.to_owned()),
        (None, None) => None,
    }
}

fn strip_trailing_season_number(value: &str) -> Option<String> {
    let mut words = value.split_whitespace().collect::<Vec<_>>();
    let last = words.last().copied()?;
    if last.chars().all(|character| character.is_ascii_digit())
        && words.len() > 1
        && words[..words.len() - 1]
            .iter()
            .any(|word| word.chars().any(is_cjk))
    {
        words.pop();
        return Some(words.join(" "));
    }
    None
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff
    )
}

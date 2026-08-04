use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use quick_xml::{events::Event, reader::Reader};
use serde_json::Value;
use tokio::{fs, process::Command, time::timeout};

use crate::{
    domain::{ids::LibraryId, time::duration_to_ticks},
    storage::{Database, MediaProbeUpdate, MediaStreamUpdate, StorageError},
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;
const MAX_XML_EVENTS: usize = 20_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaProbeResult {
    pub container: Option<String>,
    pub source_size: Option<i64>,
    pub duration_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub streams: Vec<MediaStreamResult>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaStreamResult {
    pub stream_index: i64,
    pub stream_type: StreamType,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    pub details: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamType {
    Video,
    Audio,
    Subtitle,
}

impl StreamType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "VIDEO",
            Self::Audio => "AUDIO",
            Self::Subtitle => "SUBTITLE",
        }
    }
}

pub fn parse_probe_json(bytes: &[u8]) -> Result<MediaProbeResult, ProbeError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(ProbeError::OutputTooLarge);
    }
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?;
    let object = document.as_object().ok_or_else(|| {
        ProbeError::InvalidOutput("ffprobe JSON root is not an object".to_owned())
    })?;

    let format = object.get("format").and_then(Value::as_object);
    let container = format.and_then(|value| string_field(value, "format_name"));
    let source_size = match format.and_then(|value| value.get("size")) {
        Some(value) => parse_optional_integer(value, "format.size")?,
        None => None,
    };
    let duration_ticks = match format.and_then(|value| value.get("duration")) {
        Some(value) => parse_optional_duration(value, "format.duration")?,
        None => None,
    };
    let bitrate = match format.and_then(|value| value.get("bit_rate")) {
        Some(value) => parse_optional_integer(value, "format.bit_rate")?,
        None => None,
    };

    let mut streams = Vec::new();
    let mut stream_indices = HashSet::new();
    if let Some(values) = object.get("streams") {
        let values = values.as_array().ok_or_else(|| {
            ProbeError::InvalidOutput("ffprobe streams is not an array".to_owned())
        })?;
        for (ordinal, value) in values.iter().enumerate() {
            let Some(stream) = value.as_object() else {
                return Err(ProbeError::InvalidOutput(
                    "ffprobe stream is not an object".to_owned(),
                ));
            };
            let Some(stream_type) = stream
                .get("codec_type")
                .and_then(Value::as_str)
                .and_then(parse_stream_type)
            else {
                continue;
            };
            let stream_index = stream
                .get("index")
                .map(|value| parse_integer(value, "stream.index"))
                .transpose()?
                .unwrap_or(i64::try_from(ordinal).map_err(|_| {
                    ProbeError::InvalidOutput("stream index overflows i64".to_owned())
                })?);
            if !stream_indices.insert(stream_index) {
                return Err(ProbeError::InvalidOutput(
                    "ffprobe stream indexes are duplicated".to_owned(),
                ));
            }
            let tags = stream.get("tags").and_then(Value::as_object);
            let disposition = stream.get("disposition").and_then(Value::as_object);
            streams.push(MediaStreamResult {
                stream_index,
                stream_type,
                codec: string_field(stream, "codec_name"),
                language: tags.and_then(|value| string_field(value, "language")),
                title: tags.and_then(|value| string_field(value, "title")),
                is_default: disposition
                    .and_then(|value| integer_field(value, "default"))
                    .is_some_and(|value| value != 0),
                is_forced: disposition
                    .and_then(|value| integer_field(value, "forced"))
                    .is_some_and(|value| value != 0),
                details: ffprobe_stream_details(stream),
            });
        }
    }

    Ok(MediaProbeResult {
        container,
        source_size,
        duration_ticks,
        bitrate,
        streams,
    })
}

fn parse_stream_type(value: &str) -> Option<StreamType> {
    match value {
        "video" => Some(StreamType::Video),
        "audio" => Some(StreamType::Audio),
        "subtitle" => Some(StreamType::Subtitle),
        _ => None,
    }
}

pub fn parse_media_info_json(bytes: &[u8]) -> Result<MediaProbeResult, ProbeError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(ProbeError::OutputTooLarge);
    }
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?;
    let source = if let Some(values) = document.as_array() {
        values
            .first()
            .and_then(Value::as_object)
            .and_then(|value| value.get("MediaSourceInfo"))
            .and_then(Value::as_object)
    } else {
        document
            .as_object()
            .and_then(|value| value.get("MediaSourceInfo").or(Some(&document)))
            .and_then(Value::as_object)
    }
    .ok_or_else(|| ProbeError::InvalidOutput("media info source is not an object".to_owned()))?;

    let container = string_field(source, "Container");
    let source_size = optional_integer_field(source, "Size", "MediaSourceInfo.Size")?;
    let duration_ticks =
        optional_integer_field(source, "RunTimeTicks", "MediaSourceInfo.RunTimeTicks")?;
    let bitrate = optional_integer_field(source, "Bitrate", "MediaSourceInfo.Bitrate")?;
    let values = source
        .get("MediaStreams")
        .and_then(Value::as_array)
        .ok_or_else(|| ProbeError::InvalidOutput("MediaStreams is not an array".to_owned()))?;
    let mut streams = Vec::with_capacity(values.len());
    let mut indexes = HashSet::new();
    for (ordinal, value) in values.iter().enumerate() {
        let stream = value.as_object().ok_or_else(|| {
            ProbeError::InvalidOutput("MediaStreams entry is not an object".to_owned())
        })?;
        let stream_type = stream
            .get("Type")
            .and_then(Value::as_str)
            .and_then(parse_emby_stream_type)
            .ok_or_else(|| {
                ProbeError::InvalidOutput("MediaStreams entry has no type".to_owned())
            })?;
        let stream_index = match stream.get("Index") {
            Some(value) => integer_value(value, "MediaStreams.Index")?,
            None => i64::try_from(ordinal)
                .map_err(|_| ProbeError::InvalidOutput("stream index overflows i64".to_owned()))?,
        };
        if !indexes.insert(stream_index) {
            return Err(ProbeError::InvalidOutput(
                "MediaStreams indexes are duplicated".to_owned(),
            ));
        }
        streams.push(MediaStreamResult {
            stream_index,
            stream_type,
            codec: string_field(stream, "Codec"),
            language: string_field(stream, "Language"),
            title: string_field(stream, "DisplayTitle"),
            is_default: bool_field(stream, "IsDefault"),
            is_forced: bool_field(stream, "IsForced"),
            details: media_info_stream_details(stream),
        });
    }

    Ok(MediaProbeResult {
        container,
        source_size,
        duration_ticks,
        bitrate,
        streams,
    })
}

fn parse_emby_stream_type(value: &str) -> Option<StreamType> {
    match value.to_ascii_uppercase().as_str() {
        "VIDEO" => Some(StreamType::Video),
        "AUDIO" => Some(StreamType::Audio),
        "SUBTITLE" => Some(StreamType::Subtitle),
        _ => None,
    }
}

fn optional_integer_field(
    object: &serde_json::Map<String, Value>,
    key: &str,
    field: &str,
) -> Result<Option<i64>, ProbeError> {
    match object.get(key) {
        Some(value) => {
            if value.is_null() {
                Ok(None)
            } else {
                integer_value(value, field).map(Some)
            }
        }
        None => Ok(None),
    }
}

fn integer_value(value: &Value, field: &str) -> Result<i64, ProbeError> {
    match value {
        Value::Number(value) => value
            .as_i64()
            .ok_or_else(|| ProbeError::InvalidOutput(format!("{field} is not an integer"))),
        Value::String(value) => value
            .parse::<i64>()
            .map_err(|_| ProbeError::InvalidOutput(format!("{field} is not an integer"))),
        _ => Err(ProbeError::InvalidOutput(format!(
            "{field} is not an integer"
        ))),
    }
}

fn bool_field(object: &serde_json::Map<String, Value>, key: &str) -> bool {
    match object.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(value)) => value.as_i64().is_some_and(|value| value != 0),
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes"
        ),
        _ => false,
    }
}

fn media_info_stream_details(stream: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    const FIELDS: [(&str, &str); 27] = [
        ("DisplayLanguage", "DisplayLanguage"),
        ("TimeBase", "TimeBase"),
        ("VideoRange", "VideoRange"),
        ("VideoRangeType", "VideoRangeType"),
        ("IsInterlaced", "IsInterlaced"),
        ("BitRate", "BitRate"),
        ("BitDepth", "BitDepth"),
        ("RefFrames", "RefFrames"),
        ("Height", "Height"),
        ("Width", "Width"),
        ("AverageFrameRate", "AverageFrameRate"),
        ("RealFrameRate", "RealFrameRate"),
        ("Profile", "Profile"),
        ("AspectRatio", "AspectRatio"),
        ("PixelFormat", "PixelFormat"),
        ("Level", "Level"),
        ("ChannelLayout", "ChannelLayout"),
        ("Channels", "Channels"),
        ("SampleRate", "SampleRate"),
        ("IsHearingImpaired", "IsHearingImpaired"),
        ("ColorSpace", "ColorSpace"),
        ("ColorTransfer", "ColorTransfer"),
        ("ColorPrimaries", "ColorPrimaries"),
        ("ExtendedVideoType", "ExtendedVideoType"),
        ("ExtendedVideoSubType", "ExtendedVideoSubType"),
        (
            "ExtendedVideoSubTypeDescription",
            "ExtendedVideoSubTypeDescription",
        ),
        ("IsTextSubtitleStream", "IsTextSubtitleStream"),
    ];
    copy_detail_fields(stream, &FIELDS)
}

pub fn parse_nfo_streamdetails(bytes: &[u8]) -> Result<Option<MediaProbeResult>, ProbeError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(ProbeError::OutputTooLarge);
    }
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut streamdetails_depth = None;
    let mut current_stream = None;
    let mut current_field = None;
    let mut streams = Vec::new();
    let mut duration_ticks = None;
    let mut bitrate = None;
    let mut event_count = 0;
    loop {
        event_count += 1;
        if event_count > MAX_XML_EVENTS {
            return Err(ProbeError::InvalidOutput(
                "NFO exceeds XML event limit".to_owned(),
            ));
        }
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => {
                if depth != 0 {
                    return Err(ProbeError::InvalidOutput(
                        "NFO XML is unbalanced".to_owned(),
                    ));
                }
                break;
            }
            Ok(Event::Start(event)) => {
                depth = depth.saturating_add(1);
                let name = event.name().as_ref().to_ascii_lowercase();
                if name == b"streamdetails" {
                    streamdetails_depth = Some(depth);
                } else if streamdetails_depth.is_some() && current_stream.is_none() {
                    let stream_type = match name.as_slice() {
                        b"video" => Some(StreamType::Video),
                        b"audio" => Some(StreamType::Audio),
                        b"subtitle" => Some(StreamType::Subtitle),
                        _ => None,
                    };
                    if let Some(stream_type) = stream_type {
                        let stream_index = i64::try_from(streams.len()).map_err(|_| {
                            ProbeError::InvalidOutput("stream index overflows i64".to_owned())
                        })?;
                        streams.push(MediaStreamResult {
                            stream_index,
                            stream_type,
                            codec: None,
                            language: None,
                            title: None,
                            is_default: false,
                            is_forced: false,
                            details: BTreeMap::new(),
                        });
                        current_stream = Some(streams.len() - 1);
                    }
                } else if current_stream.is_some() {
                    current_field = Some(name);
                }
            }
            Ok(Event::End(event)) => {
                let name = event.name().as_ref().to_ascii_lowercase();
                if current_stream.is_some_and(|index| {
                    matches!(
                        (streams[index].stream_type, name.as_slice()),
                        (StreamType::Video, b"video")
                            | (StreamType::Audio, b"audio")
                            | (StreamType::Subtitle, b"subtitle")
                    )
                }) {
                    current_stream = None;
                }
                if streamdetails_depth == Some(depth) && name == b"streamdetails" {
                    streamdetails_depth = None;
                }
                current_field = None;
                if depth == 0 {
                    return Err(ProbeError::InvalidOutput(
                        "NFO XML is unbalanced".to_owned(),
                    ));
                }
                depth -= 1;
            }
            Ok(Event::Text(event)) => {
                let value = event
                    .decode()
                    .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?
                    .trim()
                    .to_owned();
                assign_nfo_stream_field(
                    current_stream,
                    current_field.as_deref(),
                    &value,
                    &mut streams,
                    &mut duration_ticks,
                    &mut bitrate,
                )?;
            }
            Ok(Event::CData(event)) => {
                let value = event
                    .decode()
                    .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?
                    .trim()
                    .to_owned();
                assign_nfo_stream_field(
                    current_stream,
                    current_field.as_deref(),
                    &value,
                    &mut streams,
                    &mut duration_ticks,
                    &mut bitrate,
                )?;
            }
            Ok(Event::DocType(_)) => {
                return Err(ProbeError::InvalidOutput(
                    "NFO doctype is not allowed".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(ProbeError::InvalidOutput(error.to_string())),
        }
        buffer.clear();
    }
    if streams.is_empty() {
        return Ok(None);
    }
    Ok(Some(MediaProbeResult {
        container: None,
        source_size: None,
        duration_ticks,
        bitrate,
        streams,
    }))
}

fn assign_nfo_stream_field(
    current_stream: Option<usize>,
    field: Option<&[u8]>,
    value: &str,
    streams: &mut [MediaStreamResult],
    duration_ticks: &mut Option<i64>,
    bitrate: &mut Option<i64>,
) -> Result<(), ProbeError> {
    let Some(index) = current_stream else {
        return Ok(());
    };
    let Some(field) = field else {
        return Ok(());
    };
    if value.is_empty() {
        return Ok(());
    }
    let stream = &mut streams[index];
    match field {
        b"codec" | b"micodec" if stream.codec.is_none() => {
            stream.codec = Some(value.to_owned());
        }
        b"language" => stream.language = Some(value.to_owned()),
        b"default" => stream.is_default = parse_bool_text(value),
        b"forced" => stream.is_forced = parse_bool_text(value),
        b"durationinseconds" => {
            let seconds = value.parse::<f64>().map_err(|_| {
                ProbeError::InvalidOutput("NFO durationinseconds is not a number".to_owned())
            })?;
            if seconds.is_finite() && seconds >= 0.0 {
                let duration = Duration::try_from_secs_f64(seconds).map_err(|_| {
                    ProbeError::InvalidOutput(
                        "NFO duration is outside the supported range".to_owned(),
                    )
                })?;
                *duration_ticks = Some(
                    duration_to_ticks(duration)
                        .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?,
                );
            }
        }
        b"bitrate" => {
            let value = value.parse::<i64>().map_err(|_| {
                ProbeError::InvalidOutput("NFO bitrate is not an integer".to_owned())
            })?;
            stream
                .details
                .insert("BitRate".to_owned(), Value::from(value));
            if stream.stream_type == StreamType::Video && bitrate.is_none() {
                *bitrate = Some(value);
            }
        }
        field => {
            if let Some(key) = nfo_detail_key(field) {
                let value = if matches!(key, "Width" | "Height" | "Channels" | "SampleRate") {
                    Value::from(value.parse::<i64>().map_err(|_| {
                        ProbeError::InvalidOutput(format!("NFO {key} is not an integer"))
                    })?)
                } else if key == "RealFrameRate" {
                    Value::from(value.parse::<f64>().map_err(|_| {
                        ProbeError::InvalidOutput("NFO framerate is not a number".to_owned())
                    })?)
                } else {
                    Value::from(value.to_owned())
                };
                stream.details.insert(key.to_owned(), value);
            }
        }
    }
    Ok(())
}

fn parse_bool_text(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}

fn nfo_detail_key(field: &[u8]) -> Option<&'static str> {
    match field {
        b"width" => Some("Width"),
        b"height" => Some("Height"),
        b"aspect" | b"aspectratio" => Some("AspectRatio"),
        b"framerate" => Some("RealFrameRate"),
        b"channels" => Some("Channels"),
        b"samplingrate" => Some("SampleRate"),
        b"scantype" => Some("ScanType"),
        b"profile" => Some("Profile"),
        b"level" => Some("Level"),
        b"pixelformat" => Some("PixelFormat"),
        b"colorspace" => Some("ColorSpace"),
        b"colortransfer" => Some("ColorTransfer"),
        b"colorprimaries" => Some("ColorPrimaries"),
        _ => None,
    }
}

fn string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn integer_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<i64> {
    object.get(key).and_then(|value| match value {
        Value::Number(value) => value.as_i64(),
        Value::String(value) => value.parse().ok(),
        _ => None,
    })
}

fn ffprobe_stream_details(stream: &serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    const FIELDS: [(&str, &str); 16] = [
        ("width", "Width"),
        ("height", "Height"),
        ("display_aspect_ratio", "AspectRatio"),
        ("profile", "Profile"),
        ("level", "Level"),
        ("pix_fmt", "PixelFormat"),
        ("bit_rate", "BitRate"),
        ("bits_per_raw_sample", "BitDepth"),
        ("channels", "Channels"),
        ("channel_layout", "ChannelLayout"),
        ("sample_rate", "SampleRate"),
        ("r_frame_rate", "RealFrameRate"),
        ("avg_frame_rate", "AverageFrameRate"),
        ("color_space", "ColorSpace"),
        ("color_transfer", "ColorTransfer"),
        ("color_primaries", "ColorPrimaries"),
    ];
    copy_detail_fields(stream, &FIELDS)
}

fn copy_detail_fields(
    object: &serde_json::Map<String, Value>,
    fields: &[(&str, &str)],
) -> BTreeMap<String, Value> {
    fields
        .iter()
        .filter_map(|(source, target)| {
            let value = object.get(*source)?;
            (!value.is_null()).then(|| ((*target).to_owned(), value.clone()))
        })
        .collect()
}

fn parse_integer(value: &Value, field: &str) -> Result<i64, ProbeError> {
    let text = scalar_text(value)
        .ok_or_else(|| ProbeError::InvalidOutput(format!("{field} is not an integer")))?;
    if text == "N/A" {
        return Err(ProbeError::InvalidOutput(format!("{field} is unavailable")));
    }
    text.parse::<i64>()
        .map_err(|_| ProbeError::InvalidOutput(format!("{field} is not an integer")))
}

fn parse_optional_integer(value: &Value, field: &str) -> Result<Option<i64>, ProbeError> {
    let text = scalar_text(value)
        .ok_or_else(|| ProbeError::InvalidOutput(format!("{field} is not an integer")))?;
    if text == "N/A" {
        return Ok(None);
    }
    text.parse::<i64>()
        .map(Some)
        .map_err(|_| ProbeError::InvalidOutput(format!("{field} is not an integer")))
}

fn parse_optional_duration(value: &Value, field: &str) -> Result<Option<i64>, ProbeError> {
    let text = scalar_text(value)
        .ok_or_else(|| ProbeError::InvalidOutput(format!("{field} is not a duration")))?;
    if text == "N/A" {
        return Ok(None);
    }
    let seconds = text
        .parse::<f64>()
        .map_err(|_| ProbeError::InvalidOutput(format!("{field} is not a duration")))?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(ProbeError::InvalidOutput(format!(
            "{field} is outside the supported range"
        )));
    }
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| {
        ProbeError::InvalidOutput(format!("{field} is outside the supported range"))
    })?;
    duration_to_ticks(duration)
        .map(Some)
        .map_err(|error| ProbeError::InvalidOutput(error.to_string()))
}

fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct FfprobeRunner {
    binary: PathBuf,
    timeout: Duration,
}

impl FfprobeRunner {
    pub fn new(binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            timeout,
        }
    }

    pub fn default_binary() -> Self {
        Self::new("ffprobe", DEFAULT_TIMEOUT)
    }

    pub async fn probe_path(&self, path: &Path) -> Result<MediaProbeResult, ProbeError> {
        let child = Command::new(&self.binary)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(ProbeError::Io)?;
        let output = timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| ProbeError::Timeout)?
            .map_err(ProbeError::Io)?;
        if !output.status.success() {
            return Err(ProbeError::Exit {
                code: output.status.code(),
                stderr: truncate(&output.stderr),
            });
        }
        parse_probe_json(&output.stdout)
    }
}

impl Default for FfprobeRunner {
    fn default() -> Self {
        Self::default_binary()
    }
}

#[derive(Debug)]
pub enum ProbeError {
    Io(std::io::Error),
    Timeout,
    Exit { code: Option<i32>, stderr: String },
    OutputTooLarge,
    InvalidOutput(String),
}

impl ProbeError {
    pub fn failure_status(&self) -> &'static str {
        match self {
            Self::Timeout => "TIMEOUT",
            Self::Exit { .. } => "FAILED",
            Self::Io(_) => "FAILED",
            Self::OutputTooLarge | Self::InvalidOutput(_) => "FAILED",
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "ffprobe process failed: {error}"),
            Self::Timeout => formatter.write_str("ffprobe timed out"),
            Self::Exit { code, stderr } => {
                write!(formatter, "ffprobe exited with {:?}: {}", code, stderr)
            }
            Self::OutputTooLarge => formatter.write_str("ffprobe output exceeds size limit"),
            Self::InvalidOutput(error) => write!(formatter, "invalid ffprobe output: {error}"),
        }
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Timeout | Self::Exit { .. } | Self::OutputTooLarge | Self::InvalidOutput(_) => {
                None
            }
        }
    }
}

fn truncate(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_ERROR_BYTES)]);
    text.trim().to_owned()
}

#[derive(Clone)]
pub struct MediaProbeService {
    database: Database,
    runner: FfprobeRunner,
}

impl MediaProbeService {
    pub fn new(database: Database, runner: FfprobeRunner) -> Self {
        Self { database, runner }
    }

    pub async fn probe_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ProbeReport, ProbeServiceError> {
        let sources = self
            .database
            .list_media_sources_for_library(&library_id.to_string())
            .await?;
        let mut report = ProbeReport::default();
        for source in sources {
            if source.probe_status != "PENDING" {
                report.skipped += 1;
                continue;
            }
            report.attempted += 1;
            let path = safe_media_path(&source.root_path, &source.relative_path)?;
            match self.probe_source(&path).await {
                Ok(result) => {
                    let detail_json = result
                        .streams
                        .iter()
                        .map(|stream| {
                            if stream.details.is_empty() {
                                None
                            } else {
                                serde_json::to_string(&stream.details).ok()
                            }
                        })
                        .collect::<Vec<_>>();
                    let streams = result
                        .streams
                        .iter()
                        .zip(detail_json.iter())
                        .map(|(stream, details)| MediaStreamUpdate {
                            stream_index: stream.stream_index,
                            stream_type: stream.stream_type.as_str(),
                            codec: stream.codec.as_deref(),
                            language: stream.language.as_deref(),
                            title: stream.title.as_deref(),
                            details_json: details.as_deref(),
                            external_path: None,
                            is_external: false,
                            is_default: stream.is_default,
                            is_forced: stream.is_forced,
                        })
                        .collect::<Vec<_>>();
                    let external_subtitles =
                        discover_external_subtitles(&path, &source.root_path).await;
                    let next_stream_index = result
                        .streams
                        .iter()
                        .map(|stream| stream.stream_index)
                        .max()
                        .unwrap_or(-1)
                        .saturating_add(1);
                    let mut streams = streams;
                    for (offset, subtitle) in external_subtitles.iter().enumerate() {
                        streams.push(MediaStreamUpdate {
                            stream_index: next_stream_index
                                .saturating_add(i64::try_from(offset).unwrap_or(i64::MAX)),
                            stream_type: "SUBTITLE",
                            codec: subtitle.codec.as_deref(),
                            language: subtitle.language.as_deref(),
                            title: subtitle.title.as_deref(),
                            details_json: None,
                            external_path: Some(subtitle.relative_path.as_str()),
                            is_external: true,
                            is_default: subtitle.is_default,
                            is_forced: subtitle.is_forced,
                        });
                    }
                    self.database
                        .save_media_probe(MediaProbeUpdate {
                            source_id: &source.source_id,
                            container: result.container.as_deref(),
                            source_size: result.source_size,
                            duration_ticks: result.duration_ticks,
                            bitrate: result.bitrate,
                            streams: &streams,
                        })
                        .await?;
                    report.ready += 1;
                }
                Err(error) => {
                    if matches!(error, ProbeError::Timeout) {
                        report.timed_out += 1;
                    } else {
                        report.failed += 1;
                    }
                    self.database
                        .mark_media_probe_failed(
                            &source.source_id,
                            error.failure_status(),
                            &error.to_string(),
                        )
                        .await?;
                }
            }
        }
        Ok(report)
    }

    async fn probe_source(&self, path: &Path) -> Result<MediaProbeResult, ProbeError> {
        if is_strm_path(path) {
            if let Some(result) = read_media_info_sidecar(path).await {
                return Ok(result);
            }
            if let Some(result) = read_nfo_streamdetails(path).await {
                return Ok(result);
            }
        }
        match self.runner.probe_path(path).await {
            Ok(result) => Ok(result),
            Err(error) => read_media_info_sidecar(path)
                .await
                .or(read_nfo_streamdetails(path).await)
                .ok_or(error),
        }
    }
}

fn is_strm_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

async fn read_media_info_sidecar(path: &Path) -> Option<MediaProbeResult> {
    let stem = path.file_stem()?.to_str()?;
    let sidecar = path.with_file_name(format!("{stem}-mediainfo.json"));
    let bytes = fs::read(sidecar).await.ok()?;
    parse_media_info_json(&bytes).ok()
}

async fn read_nfo_streamdetails(path: &Path) -> Option<MediaProbeResult> {
    let nfo = path.with_extension("nfo");
    let bytes = fs::read(nfo).await.ok()?;
    parse_nfo_streamdetails(&bytes).ok().flatten()
}

fn safe_media_path(root_path: &str, relative_path: &str) -> Result<PathBuf, ProbeServiceError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(ProbeServiceError::InvalidSourcePath {
            root_path: root_path.to_owned(),
            relative_path: relative_path.to_owned(),
        });
    }
    Ok(PathBuf::from(root_path).join(relative))
}

#[derive(Clone, Debug)]
struct ExternalSubtitle {
    relative_path: String,
    codec: Option<String>,
    language: Option<String>,
    title: Option<String>,
    is_default: bool,
    is_forced: bool,
}

async fn discover_external_subtitles(media_path: &Path, root_path: &str) -> Vec<ExternalSubtitle> {
    let Some(directory) = media_path.parent() else {
        return Vec::new();
    };
    let Some(media_stem) = media_path.file_stem().and_then(|value| value.to_str()) else {
        return Vec::new();
    };
    let Ok(mut entries) = fs::read_dir(directory).await else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let supported = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "srt" | "ass" | "ssa" | "vtt" | "sub" | "sup"
                )
            });
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if supported && (stem == media_stem || stem.starts_with(&format!("{media_stem}."))) {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let stem = path.file_stem()?.to_str()?;
            let suffix = stem.strip_prefix(media_stem).unwrap_or_default();
            let tokens = suffix
                .trim_start_matches('.')
                .split(['.', '_', '-'])
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            let language = tokens.iter().find_map(|token| subtitle_language(token));
            let is_forced = tokens
                .iter()
                .any(|token| token.eq_ignore_ascii_case("forced"));
            let is_default = tokens
                .iter()
                .any(|token| token.eq_ignore_ascii_case("default"));
            let codec = path
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase());
            let relative_path = path.strip_prefix(root_path).ok()?.to_str()?.to_owned();
            Some(ExternalSubtitle {
                relative_path,
                codec,
                language,
                title: (!tokens.is_empty()).then(|| tokens.join(" ")),
                is_default,
                is_forced,
            })
        })
        .collect()
}

fn subtitle_language(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "en" | "eng" => Some("eng".to_owned()),
        "zh" | "chi" | "cmn" | "chs" | "zh-cn" | "zh-hans" => Some("zho".to_owned()),
        "cht" | "zh-tw" | "zh-hant" => Some("zho".to_owned()),
        "ja" | "jpn" => Some("jpn".to_owned()),
        "ko" | "kor" => Some("kor".to_owned()),
        "fr" | "fra" | "fre" => Some("fra".to_owned()),
        "de" | "deu" | "ger" => Some("deu".to_owned()),
        "es" | "spa" => Some("spa".to_owned()),
        "it" | "ita" => Some("ita".to_owned()),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProbeReport {
    pub attempted: usize,
    pub ready: usize,
    pub failed: usize,
    pub timed_out: usize,
    pub skipped: usize,
}

#[derive(Debug)]
pub enum ProbeServiceError {
    InvalidSourcePath {
        root_path: String,
        relative_path: String,
    },
    Storage(StorageError),
}

impl fmt::Display for ProbeServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourcePath {
                root_path,
                relative_path,
            } => write!(
                formatter,
                "media source path '{}' escapes root '{}'",
                relative_path, root_path
            ),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProbeServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSourcePath { .. } => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for ProbeServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

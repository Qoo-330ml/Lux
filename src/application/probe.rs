use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use quick_xml::{events::Event, reader::Reader};
use serde_json::{Map, Value};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, OnceCell},
    task::JoinSet,
    time::{MissedTickBehavior, interval_at},
};

use crate::{
    application::nfo::{NfoWriteError, NfoWriteService},
    application::plugin_protocol::{MediaProbeRpcResult, MediaProbeRpcStreamType},
    config::probe_concurrency_override_from_env,
    domain::{ids::LibraryId, time::duration_to_ticks},
    observability::resources::{ResourceMetrics, automatic_probe_concurrency, probe_cpu_capacity},
    storage::{Database, MediaProbeUpdate, MediaStreamUpdate, StorageError, StoredMediaSourcePath},
};

const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_XML_EVENTS: usize = 20_000;
const LIBRARY_SOURCE_PAGE_SIZE: usize = 500;
const PROBE_ADMISSION_TICK: Duration = Duration::from_secs(1);
pub(crate) const MAX_EFFECTIVE_PROBE_CONCURRENCY: usize = 512;

mod runner;
use runner::read_limited;
pub use runner::{FfprobeRunner, ProbeError};
mod adaptive;
mod concurrency;

pub(crate) async fn shutdown_probe_runtime() -> Result<(), ProbeServiceError> {
    concurrency::shutdown_global_runtime()
        .await
        .map_err(|error| ProbeServiceError::Worker(error.to_string()))
}

fn process_probe_hard_cap() -> Result<Option<usize>, String> {
    match probe_concurrency_override_from_env().map_err(|error| error.to_string())? {
        Some(value) => usize::try_from(value)
            .map(Some)
            .map_err(|_| format!("LUX_PROBE_CONCURRENCY '{value}' is not supported")),
        None => Ok(None),
    }
}

fn resolve_process_probe_budget(override_budget: Option<usize>, automatic_budget: usize) -> usize {
    override_budget
        .unwrap_or(automatic_budget)
        .clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaProbeResult {
    pub container: Option<String>,
    pub source_size: Option<i64>,
    pub duration_ticks: Option<i64>,
    pub bitrate: Option<i64>,
    pub streams: Vec<MediaStreamResult>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MediaInfoSidecarContext {
    pub(crate) protocol: &'static str,
    pub(crate) is_remote: bool,
    pub(crate) supports_transcoding: bool,
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

pub fn media_probe_result_from_rpc(response: MediaProbeRpcResult) -> MediaProbeResult {
    MediaProbeResult {
        container: response.container,
        source_size: response.source_size,
        duration_ticks: response.duration_ticks,
        bitrate: response.bitrate,
        streams: response
            .streams
            .into_iter()
            .map(|stream| MediaStreamResult {
                stream_index: stream.stream_index,
                stream_type: match stream.stream_type {
                    MediaProbeRpcStreamType::Video => StreamType::Video,
                    MediaProbeRpcStreamType::Audio => StreamType::Audio,
                    MediaProbeRpcStreamType::Subtitle => StreamType::Subtitle,
                },
                codec: stream.codec,
                language: stream.language,
                title: stream.title,
                is_default: stream.is_default,
                is_forced: stream.is_forced,
                details: stream.details,
            })
            .collect(),
    }
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
            let disposition = stream.get("disposition").and_then(Value::as_object);
            if disposition
                .and_then(|value| integer_field(value, "attached_pic"))
                .is_some_and(|value| value != 0)
            {
                continue;
            }
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
            let mut details = ffprobe_stream_details(stream);
            if let Some(disposition) = disposition
                && disposition.contains_key("hearing_impaired")
            {
                details.insert(
                    "IsHearingImpaired".to_owned(),
                    Value::Bool(bool_field(disposition, "hearing_impaired")),
                );
            }
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
                details,
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
    const FIELDS: [(&str, &str); 17] = [
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
        ("field_order", "ScanType"),
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

#[derive(Clone)]
pub struct MediaProbeService {
    database: Database,
    runner: FfprobeRunner,
    nfo_writer: Option<NfoWriteService>,
    resources: ResourceMetrics,
    process_hard_cap: Result<Option<usize>, String>,
}

type ProbeTaskResult = (
    StoredMediaSourcePath,
    PathBuf,
    Result<Option<MediaProbeResult>, ProbeError>,
);

fn nfo_write_error_code(error: &NfoWriteError) -> &'static str {
    match error {
        NfoWriteError::Nfo(_) => "INVALID_NFO",
        NfoWriteError::ItemNotFound => "ITEM_NOT_FOUND",
        NfoWriteError::InvalidMetadata(_) => "INVALID_METADATA",
        NfoWriteError::InvalidXml(_) => "INVALID_XML",
        NfoWriteError::Io { .. } => "IO",
        NfoWriteError::SymlinkTarget(_) => "SYMLINK_TARGET",
        NfoWriteError::PathOutsideRoot(_) => "PATH_OUTSIDE_ROOT",
        NfoWriteError::ConcurrentModification(_) => "CONCURRENT_MODIFICATION",
        NfoWriteError::TooLarge(_) => "NFO_TOO_LARGE",
        NfoWriteError::Storage(_) => "STORAGE",
    }
}

#[derive(Default)]
struct SubtitleDirectoryCache {
    directories: AsyncMutex<HashMap<PathBuf, Arc<OnceCell<Vec<PathBuf>>>>>,
}

impl SubtitleDirectoryCache {
    async fn paths_for(&self, directory: &Path) -> Vec<PathBuf> {
        let cell = {
            let mut directories = self.directories.lock().await;
            directories
                .entry(directory.to_path_buf())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let directory = directory.to_owned();
        cell.get_or_init(|| async move { enumerate_external_subtitle_paths(&directory).await })
            .await
            .clone()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProbeTargetState {
    Done,
    Failed,
    Skipped,
}

impl ProbeTargetState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "DONE",
            Self::Failed => "FAILED",
            Self::Skipped => "SKIPPED",
        }
    }
}

impl MediaProbeService {
    pub fn new(database: Database, runner: FfprobeRunner) -> Self {
        let process_hard_cap = process_probe_hard_cap();
        Self {
            database,
            runner,
            nfo_writer: None,
            resources: ResourceMetrics::new(),
            process_hard_cap,
        }
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_nfo_writer(mut self, nfo_writer: NfoWriteService) -> Self {
        self.nfo_writer = Some(nfo_writer);
        self
    }

    pub(crate) async fn import_media_info_for_absolute_path(
        &self,
        absolute_path: &str,
        result: &MediaProbeResult,
        chapters: &[crate::storage::MediaInfoChapterUpdate],
    ) -> Result<(), ProbeServiceError> {
        let source = self
            .database
            .find_strm_source_by_absolute_path(absolute_path)
            .await?
            .ok_or_else(|| ProbeServiceError::Worker("STRM source was not indexed".to_owned()))?;
        let details = result
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
            .zip(details.iter())
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
        self.database
            .save_media_probe(MediaProbeUpdate {
                source_id: &source.source_id,
                container: result.container.as_deref(),
                source_size: result.source_size,
                duration_ticks: result.duration_ticks,
                bitrate: result.bitrate,
                streams: &streams,
                chapters,
            })
            .await?;
        Ok(())
    }

    pub async fn probe_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ProbeReport, ProbeServiceError> {
        let mut report = ProbeReport::default();
        let library_id_text = library_id.to_string();
        let configured = self.configured_concurrency(&library_id_text).await?;
        let subtitle_cache = Arc::new(SubtitleDirectoryCache::default());
        let mut offset = 0_i64;
        loop {
            let sources = self
                .database
                .list_media_sources_for_library_page(
                    &library_id_text,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = sources.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.probe_source_page(
                sources,
                library_id,
                configured,
                &mut report,
                Arc::clone(&subtitle_cache),
            )
            .await?;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    pub async fn probe_scan_job(
        &self,
        job_id: &str,
        library_id: &str,
    ) -> Result<ProbeReport, ProbeServiceError> {
        let configured = self.configured_concurrency(library_id).await?;
        let library_id = library_id
            .parse::<LibraryId>()
            .map_err(|error| ProbeServiceError::Worker(format!("invalid library id: {error}")))?;
        let subtitle_cache = Arc::new(SubtitleDirectoryCache::default());
        let mut report = ProbeReport::default();
        loop {
            let sources = self
                .database
                .list_scan_job_target_sources_page(job_id, LIBRARY_SOURCE_PAGE_SIZE as i64, 0)
                .await?;
            if sources.is_empty() {
                break;
            }
            let states = self
                .probe_source_page(
                    sources,
                    library_id,
                    configured,
                    &mut report,
                    Arc::clone(&subtitle_cache),
                )
                .await?;
            for state in [
                ProbeTargetState::Done,
                ProbeTargetState::Failed,
                ProbeTargetState::Skipped,
            ] {
                let target_ids = states
                    .iter()
                    .filter(|(_, target_state)| *target_state == state)
                    .map(|(source_id, _)| source_id.clone())
                    .collect::<Vec<_>>();
                self.database
                    .mark_scan_job_target_stage(
                        job_id,
                        "SOURCE",
                        &target_ids,
                        "PROBE",
                        state.as_str(),
                    )
                    .await?;
            }
        }
        Ok(report)
    }

    async fn configured_concurrency(&self, library_id: &str) -> Result<usize, ProbeServiceError> {
        // This is the persisted library/request setting. It intentionally
        // remains separate from the process hard cap and runtime backpressure.
        let automatic_default = automatic_probe_concurrency().await;
        Ok(self
            .database
            .find_library(library_id)
            .await?
            .and_then(|library| usize::try_from(library.probe_concurrency).ok())
            .unwrap_or(automatic_default)
            .clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY))
    }

    async fn global_probe_runtime(&self) -> Result<concurrency::ProbeRuntime, ProbeServiceError> {
        let override_budget = *self
            .process_hard_cap
            .as_ref()
            .map_err(|error| ProbeServiceError::Worker(error.clone()))?;
        let capacity = probe_cpu_capacity().await;
        let automatic_budget = match override_budget {
            Some(budget) => budget,
            None => automatic_probe_concurrency().await,
        };
        let budget = resolve_process_probe_budget(override_budget, automatic_budget);
        let starting_limit = capacity
            .effective_cores
            .ceil()
            .clamp(1.0, MAX_EFFECTIVE_PROBE_CONCURRENCY as f64)
            as usize;
        tracing::debug!(
            cpu_capacity_cores = capacity.effective_cores,
            cpu_capacity_source = ?capacity.source,
            process_budget = budget,
            starting_limit,
            "initialized CPU-aware probe admission"
        );
        concurrency::global_runtime(budget, starting_limit, Instant::now())
            .map_err(|error| ProbeServiceError::Worker(error.to_string()))
    }

    async fn update_probe_pressure(
        &self,
        session: &concurrency::ProbeSession,
    ) -> Result<(), ProbeServiceError> {
        let pressure = self.resources.probe_pressure().await;
        session
            .update_pressure(pressure, Instant::now())
            .map_err(|error| ProbeServiceError::Worker(error.to_string()))
    }

    async fn probe_source_page(
        &self,
        sources: Vec<StoredMediaSourcePath>,
        library_id: LibraryId,
        configured_limit: usize,
        report: &mut ProbeReport,
        subtitle_cache: Arc<SubtitleDirectoryCache>,
    ) -> Result<Vec<(String, ProbeTargetState)>, ProbeServiceError> {
        let mut states = Vec::new();
        let mut local_inputs = VecDeque::new();
        let mut sidecar_inputs = VecDeque::new();
        for source in sources {
            let path = safe_media_path(&source.root_path, &source.relative_path)?;
            if is_strm_path(&path) {
                if source.probe_status != "PENDING"
                    && !(source.probe_status == "NOT_CONFIGURED" && has_strm_sidecar(&path).await)
                {
                    report.skipped += 1;
                    states.push((source.source_id, ProbeTargetState::Skipped));
                    continue;
                }
                report.attempted += 1;
                sidecar_inputs.push_back((source, path));
            } else if source.probe_status == "PENDING" {
                report.attempted += 1;
                local_inputs.push_back((source, path));
            } else {
                report.skipped += 1;
                states.push((source.source_id, ProbeTargetState::Skipped));
            }
        }

        let runtime = self.global_probe_runtime().await?;
        let session = runtime
            .register(
                library_id,
                configured_limit,
                local_inputs.len(),
                Instant::now(),
            )
            .map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
        self.update_probe_pressure(&session).await?;
        let page_worker_limit = configured_limit
            .clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY)
            .min(LIBRARY_SOURCE_PAGE_SIZE);
        let mut pending = JoinSet::<ProbeTaskResult>::new();
        let first_tick = tokio::time::Instant::now() + PROBE_ADMISSION_TICK;
        let mut admission_ticks = interval_at(first_tick, PROBE_ADMISSION_TICK);
        admission_ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            if runtime
                .is_closing()
                .map_err(|error| ProbeServiceError::Worker(error.to_string()))?
            {
                return Err(ProbeServiceError::Worker(
                    "probe admission is closing".to_owned(),
                ));
            }
            let mut blocked_version = None;
            while pending.len() < page_worker_limit {
                if !local_inputs.is_empty() {
                    let observed_version = runtime
                        .version()
                        .map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
                    let permit = match session.try_acquire(Instant::now()) {
                        Ok(Some(permit)) => permit,
                        Ok(None) => {
                            blocked_version = Some(observed_version);
                            let Some((source, path)) = sidecar_inputs.pop_front() else {
                                break;
                            };
                            let service = self.clone();
                            pending.spawn(async move {
                                let result = service.probe_strm_sidecars(&path).await;
                                (source, path, result)
                            });
                            continue;
                        }
                        Err(error) => {
                            return Err(ProbeServiceError::Worker(error.to_string()));
                        }
                    };
                    let Some((source, path)) = local_inputs.pop_front() else {
                        return Err(ProbeServiceError::Worker(
                            "local probe input disappeared after admission".to_owned(),
                        ));
                    };
                    let service = self.clone();
                    pending.spawn(async move {
                        let result = service.probe_source_with_permit(&path, permit).await;
                        (source, path, result)
                    });
                    blocked_version = None;
                    continue;
                }
                if let Some((source, path)) = sidecar_inputs.pop_front() {
                    let service = self.clone();
                    pending.spawn(async move {
                        let result = service.probe_strm_sidecars(&path).await;
                        (source, path, result)
                    });
                    continue;
                }
                break;
            }

            if pending.is_empty() && local_inputs.is_empty() && sidecar_inputs.is_empty() {
                break;
            }

            let observed_version = blocked_version.unwrap_or(
                runtime
                    .version()
                    .map_err(|error| ProbeServiceError::Worker(error.to_string()))?,
            );
            if pending.is_empty() {
                tokio::select! {
                    changed = runtime.wait_for_change_since(observed_version) => {
                        changed.map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
                    }
                    _ = admission_ticks.tick() => {
                        self.update_probe_pressure(&session).await?;
                        runtime
                            .tick(Instant::now())
                            .map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
                    }
                }
                continue;
            }

            tokio::select! {
                joined = pending.join_next() => {
                    let joined = joined.ok_or_else(|| {
                        ProbeServiceError::Worker("probe task set was empty".to_owned())
                    })?;
                    states.push(
                        self.persist_joined_probe_task(
                            joined,
                            report,
                            subtitle_cache.as_ref(),
                        )
                        .await?,
                    );
                }
                changed = runtime.wait_for_change_since(observed_version) => {
                    changed.map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
                }
                _ = admission_ticks.tick() => {
                    self.update_probe_pressure(&session).await?;
                    runtime
                        .tick(Instant::now())
                        .map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
                }
            }
        }
        Ok(states)
    }

    async fn persist_joined_probe_task(
        &self,
        joined: Result<ProbeTaskResult, tokio::task::JoinError>,
        report: &mut ProbeReport,
        subtitle_cache: &SubtitleDirectoryCache,
    ) -> Result<(String, ProbeTargetState), ProbeServiceError> {
        let task = joined.map_err(|error| ProbeServiceError::Worker(error.to_string()))?;
        let state = self
            .persist_probe_attempt(&task.0, &task.1, task.2, report, subtitle_cache)
            .await?;
        Ok((task.0.source_id.clone(), state))
    }

    async fn persist_probe_attempt(
        &self,
        source: &StoredMediaSourcePath,
        path: &Path,
        result: Result<Option<MediaProbeResult>, ProbeError>,
        report: &mut ProbeReport,
        subtitle_cache: &SubtitleDirectoryCache,
    ) -> Result<ProbeTargetState, ProbeServiceError> {
        match result {
            Ok(Some(result)) => {
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
                    discover_external_subtitles(path, &source.root_path, subtitle_cache).await;
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
                        chapters: &[],
                    })
                    .await?;
                if let Some(nfo_writer) = &self.nfo_writer
                    && let Err(error) = nfo_writer
                        .write_item_probe_details(&source.item_id, &source.source_id, &result)
                        .await
                {
                    tracing::warn!(
                        item_id = %source.item_id,
                        error_code = nfo_write_error_code(&error),
                        "NFO stream details could not be updated after media probe"
                    );
                }
                report.ready += 1;
                Ok(ProbeTargetState::Done)
            }
            Ok(None) => {
                // A selected STRM source remains pending for its persisted plugin job.
                report.skipped += 1;
                Ok(ProbeTargetState::Skipped)
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
                Ok(ProbeTargetState::Failed)
            }
        }
    }

    async fn probe_source_with_permit(
        &self,
        path: &Path,
        permit: concurrency::ProbePermit,
    ) -> Result<Option<MediaProbeResult>, ProbeError> {
        match self.runner.probe_path_with_permit(path, permit).await {
            Ok(result) => Ok(Some(result)),
            Err(error) => read_media_info_sidecar(path)
                .await
                .or(read_nfo_streamdetails(path).await)
                .map(Some)
                .ok_or(error),
        }
    }

    async fn probe_strm_sidecars(
        &self,
        path: &Path,
    ) -> Result<Option<MediaProbeResult>, ProbeError> {
        Ok(read_media_info_sidecar(path)
            .await
            .or(read_nfo_streamdetails(path).await))
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
    read_media_info_sidecar_file(&sidecar).await.ok()
}

async fn has_strm_sidecar(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    let media_info = path.with_file_name(format!("{stem}-mediainfo.json"));
    let nfo = path.with_extension("nfo");
    fs::try_exists(media_info).await.unwrap_or(false) || fs::try_exists(nfo).await.unwrap_or(false)
}

pub(crate) async fn read_media_info_sidecar_file(
    path: &Path,
) -> Result<MediaProbeResult, ProbeError> {
    let file = fs::File::open(path).await.map_err(ProbeError::Io)?;
    let bytes = read_limited(file, MAX_OUTPUT_BYTES).await?;
    parse_media_info_json(&bytes)
}

pub(crate) async fn read_media_info_sidecar_from_root(
    root_path: &str,
    relative_path: &str,
) -> Result<MediaProbeResult, ProbeError> {
    let path = safe_media_path(root_path, relative_path).map_err(|_| {
        ProbeError::InvalidOutput("media info path is outside its library root".to_owned())
    })?;
    let root = fs::canonicalize(root_path).await.map_err(ProbeError::Io)?;
    let path = fs::canonicalize(path).await.map_err(ProbeError::Io)?;
    if !path.starts_with(root) {
        return Err(ProbeError::InvalidOutput(
            "media info path is outside its library root".to_owned(),
        ));
    }
    read_media_info_sidecar_file(&path).await
}

async fn read_existing_media_info_sidecar(target: &Path) -> Result<Option<Value>, ProbeError> {
    let mut file = match fs::File::open(target).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ProbeError::Io(error)),
    };
    let bytes = read_limited(&mut file, MAX_OUTPUT_BYTES).await?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?;
    parse_media_info_json(&bytes)?;
    Ok(Some(document))
}

fn media_info_stream_value(stream: &MediaStreamResult) -> Value {
    let mut value = Map::new();
    value.insert("Index".to_owned(), Value::from(stream.stream_index));
    value.insert("Type".to_owned(), Value::from(stream.stream_type.as_str()));
    if let Some(codec) = &stream.codec {
        value.insert("Codec".to_owned(), Value::from(codec.clone()));
    }
    if let Some(language) = &stream.language {
        value.insert("Language".to_owned(), Value::from(language.clone()));
    }
    if let Some(title) = &stream.title {
        value.insert("DisplayTitle".to_owned(), Value::from(title.clone()));
    }
    value.insert("IsDefault".to_owned(), Value::from(stream.is_default));
    value.insert("IsForced".to_owned(), Value::from(stream.is_forced));
    for (key, detail) in &stream.details {
        value.insert(key.clone(), detail.clone());
    }
    Value::Object(value)
}

fn apply_media_info_source_context(
    source: &mut Map<String, Value>,
    context: MediaInfoSidecarContext,
) {
    source.insert("Protocol".to_owned(), Value::from(context.protocol));
    source.insert("Type".to_owned(), Value::from("Default"));
    source.insert("IsRemote".to_owned(), Value::from(context.is_remote));
    source.insert("HasMixedProtocols".to_owned(), Value::from(false));
    source.insert(
        "SupportsTranscoding".to_owned(),
        Value::from(context.supports_transcoding),
    );
    source.insert("SupportsDirectStream".to_owned(), Value::from(true));
    source.insert("SupportsDirectPlay".to_owned(), Value::from(true));
    source.insert("SupportsProbing".to_owned(), Value::from(true));
    source.insert("IsInfiniteStream".to_owned(), Value::from(false));
    source.insert("RequiresOpening".to_owned(), Value::from(false));
    source.insert("RequiresClosing".to_owned(), Value::from(false));
    source.insert("RequiresLooping".to_owned(), Value::from(false));
    source.insert("ReadAtNativeFramerate".to_owned(), Value::from(false));
    source.insert("RequiredHttpHeaders".to_owned(), Value::Object(Map::new()));
    source.insert("AddApiKeyToDirectStreamUrl".to_owned(), Value::from(false));
    source.insert("Formats".to_owned(), Value::Array(Vec::new()));
}

fn apply_missing_media_info_source_context(
    source: &mut Map<String, Value>,
    context: MediaInfoSidecarContext,
) {
    let mut context_fields = Map::new();
    apply_media_info_source_context(&mut context_fields, context);
    for (key, value) in context_fields {
        source.entry(key).or_insert(value);
    }
}

fn media_info_source_value(
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Value {
    let mut source = Map::new();
    source.insert(
        "Container".to_owned(),
        result
            .container
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    source.insert(
        "Size".to_owned(),
        result.source_size.map(Value::from).unwrap_or(Value::Null),
    );
    source.insert(
        "RunTimeTicks".to_owned(),
        result
            .duration_ticks
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    source.insert(
        "Bitrate".to_owned(),
        result.bitrate.map(Value::from).unwrap_or(Value::Null),
    );
    source.insert(
        "MediaStreams".to_owned(),
        Value::Array(result.streams.iter().map(media_info_stream_value).collect()),
    );
    if let Some(context) = context {
        apply_media_info_source_context(&mut source, context);
    }
    Value::Object(source)
}

fn media_info_document_value(
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Value {
    Value::Array(vec![serde_json::json!({
        "MediaSourceInfo": media_info_source_value(result, context),
        "Chapters": [],
    })])
}

fn merge_media_info_streams(existing: &[Value], streams: &[MediaStreamResult]) -> Vec<Value> {
    let mut existing_by_index = HashMap::new();
    for (ordinal, value) in existing.iter().enumerate() {
        let Some(object) = value.as_object() else {
            continue;
        };
        let index = object
            .get("Index")
            .and_then(Value::as_i64)
            .or_else(|| i64::try_from(ordinal).ok());
        if let Some(index) = index {
            existing_by_index.insert(index, object.clone());
        }
    }

    streams
        .iter()
        .map(|stream| {
            let mut value = existing_by_index
                .remove(&stream.stream_index)
                .unwrap_or_default();
            value.insert("Index".to_owned(), Value::from(stream.stream_index));
            value.insert("Type".to_owned(), Value::from(stream.stream_type.as_str()));
            match &stream.codec {
                Some(codec) => {
                    value.insert("Codec".to_owned(), Value::from(codec.clone()));
                }
                None => {
                    value.remove("Codec");
                }
            }
            match &stream.language {
                Some(language) => {
                    value.insert("Language".to_owned(), Value::from(language.clone()));
                }
                None => {
                    value.remove("Language");
                }
            }
            match &stream.title {
                Some(title) => {
                    value.insert("DisplayTitle".to_owned(), Value::from(title.clone()));
                }
                None => {
                    value.remove("DisplayTitle");
                }
            }
            value.insert("IsDefault".to_owned(), Value::from(stream.is_default));
            value.insert("IsForced".to_owned(), Value::from(stream.is_forced));
            for (key, detail) in &stream.details {
                value.insert(key.clone(), detail.clone());
            }
            Value::Object(value)
        })
        .collect()
}

fn merge_media_info_source(
    source: &mut Map<String, Value>,
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Result<(), ProbeError> {
    source.insert(
        "Container".to_owned(),
        result
            .container
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    source.insert(
        "Size".to_owned(),
        result.source_size.map(Value::from).unwrap_or(Value::Null),
    );
    source.insert(
        "RunTimeTicks".to_owned(),
        result
            .duration_ticks
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    source.insert(
        "Bitrate".to_owned(),
        result.bitrate.map(Value::from).unwrap_or(Value::Null),
    );
    let existing_streams = source
        .get("MediaStreams")
        .and_then(Value::as_array)
        .ok_or_else(|| ProbeError::InvalidOutput("MediaStreams is not an array".to_owned()))?;
    let streams = merge_media_info_streams(existing_streams, &result.streams);
    source.insert("MediaStreams".to_owned(), Value::Array(streams));
    if let Some(context) = context {
        apply_missing_media_info_source_context(source, context);
    }
    Ok(())
}

fn merge_media_info_document(
    mut document: Value,
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Result<Value, ProbeError> {
    match &mut document {
        Value::Array(values) => {
            let first = values.first_mut().ok_or_else(|| {
                ProbeError::InvalidOutput("media info sidecar array is empty".to_owned())
            })?;
            let wrapper = first.as_object_mut().ok_or_else(|| {
                ProbeError::InvalidOutput("media info sidecar entry is not an object".to_owned())
            })?;
            let source = wrapper
                .get_mut("MediaSourceInfo")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    ProbeError::InvalidOutput(
                        "media info sidecar source is not an object".to_owned(),
                    )
                })?;
            merge_media_info_source(source, result, context)?;
        }
        Value::Object(object) => {
            if let Some(source) = object
                .get_mut("MediaSourceInfo")
                .and_then(Value::as_object_mut)
            {
                merge_media_info_source(source, result, context)?;
            } else {
                merge_media_info_source(object, result, context)?;
            }
        }
        _ => {
            return Err(ProbeError::InvalidOutput(
                "media info sidecar root is not an object or array".to_owned(),
            ));
        }
    }
    Ok(document)
}

pub(crate) fn serialize_media_info_sidecar_with_context(
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&media_info_document_value(result, context))
}

pub(crate) async fn write_media_info_sidecar_with_context(
    path: &Path,
    result: &MediaProbeResult,
    context: Option<MediaInfoSidecarContext>,
) -> Result<(), ProbeError> {
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| ProbeError::InvalidOutput("media path has no valid file stem".to_owned()))?;
    let target = path.with_file_name(format!("{stem}-mediainfo.json"));
    let temporary =
        target.with_file_name(format!(".{stem}-mediainfo.{}.tmp", uuid::Uuid::now_v7()));
    let contents = match read_existing_media_info_sidecar(&target).await? {
        Some(document) => {
            let document = merge_media_info_document(document, result, context)?;
            serde_json::to_vec_pretty(&document)
                .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?
        }
        None => serialize_media_info_sidecar_with_context(result, context)
            .map_err(|error| ProbeError::InvalidOutput(error.to_string()))?,
    };
    let write_result = async {
        let mut file = fs::File::create(&temporary).await?;
        file.write_all(&contents).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, &target).await
    }
    .await;
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    write_result.map_err(ProbeError::Io)
}

async fn read_nfo_streamdetails(path: &Path) -> Option<MediaProbeResult> {
    let nfo = path.with_extension("nfo");
    let bytes = fs::read(nfo).await.ok()?;
    parse_nfo_streamdetails(&bytes).ok().flatten()
}

pub(crate) fn safe_media_path(
    root_path: &str,
    relative_path: &str,
) -> Result<PathBuf, ProbeServiceError> {
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
pub(crate) struct ExternalSubtitle {
    pub(crate) relative_path: String,
    pub(crate) codec: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

async fn enumerate_external_subtitle_paths(directory: &Path) -> Vec<PathBuf> {
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
        if supported {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

async fn discover_external_subtitles(
    media_path: &Path,
    root_path: &str,
    cache: &SubtitleDirectoryCache,
) -> Vec<ExternalSubtitle> {
    let Some(directory) = media_path.parent() else {
        return Vec::new();
    };
    let paths = cache.paths_for(directory).await;
    paths
        .into_iter()
        .filter_map(|path| external_subtitle_for_media(media_path, &path, root_path))
        .collect()
}

pub(crate) fn is_external_subtitle_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "srt" | "ass" | "ssa" | "vtt" | "sub" | "sup"
            )
        })
}

pub(crate) fn external_subtitle_for_media(
    media_path: &Path,
    subtitle_path: &Path,
    root_path: &str,
) -> Option<ExternalSubtitle> {
    if !is_external_subtitle_path(subtitle_path) {
        return None;
    }
    let media_stem = media_path.file_stem()?.to_str()?;
    let subtitle_stem = subtitle_path.file_stem()?.to_str()?;
    if subtitle_stem != media_stem && !subtitle_stem.starts_with(&format!("{media_stem}.")) {
        return None;
    }
    let suffix = subtitle_stem.strip_prefix(media_stem).unwrap_or_default();
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
    let codec = subtitle_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase());
    let relative_path = subtitle_path
        .strip_prefix(root_path)
        .ok()?
        .to_str()?
        .to_owned();
    Some(ExternalSubtitle {
        relative_path,
        codec,
        language,
        title: (!tokens.is_empty()).then(|| tokens.join(" ")),
        is_default,
        is_forced,
    })
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
    Worker(String),
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
            Self::Worker(error) => write!(formatter, "probe worker failed: {error}"),
        }
    }
}

impl std::error::Error for ProbeServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidSourcePath { .. } | Self::Worker(_) => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for ProbeServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{
        MediaInfoSidecarContext, MediaProbeResult, MediaStreamResult, ProbeError, StreamType,
        SubtitleDirectoryCache, resolve_process_probe_budget,
        serialize_media_info_sidecar_with_context, write_media_info_sidecar_with_context,
    };

    #[test]
    fn process_probe_budget_uses_auto_capacity_unless_explicitly_overridden() {
        assert_eq!(resolve_process_probe_budget(None, 40), 40);
        assert_eq!(resolve_process_probe_budget(Some(8), 40), 8);
        assert_eq!(resolve_process_probe_budget(Some(900), 40), 512);
        assert_eq!(resolve_process_probe_budget(Some(0), 40), 1);
    }

    #[tokio::test]
    async fn subtitle_directory_cache_reuses_one_listing_for_an_operation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = temporary.path().join("Movie.en.srt");
        let second = temporary.path().join("Movie.zh.srt");
        tokio::fs::write(&first, b"first")
            .await
            .expect("first subtitle");

        let cache = SubtitleDirectoryCache::default();
        let initial = cache.paths_for(temporary.path()).await;
        tokio::fs::write(&second, b"second")
            .await
            .expect("second subtitle");
        let reused = cache.paths_for(temporary.path()).await;

        assert_eq!(initial, vec![first]);
        assert_eq!(reused, initial);
    }

    #[tokio::test]
    async fn sidecar_write_preserves_external_fields_and_strm_bytes() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let strm = temporary.path().join("Movie.strm");
        let original_strm = b"https://media.example.invalid/movie.mkv\n";
        tokio::fs::write(&strm, original_strm)
            .await
            .expect("write strm");
        let sidecar = temporary.path().join("Movie-mediainfo.json");
        tokio::fs::write(
            &sidecar,
            serde_json::to_vec(&json!([{
                "MediaSourceInfo": {
                    "Container": "old",
                    "Size": 9,
                    "RunTimeTicks": 10,
                    "Bitrate": 11,
                    "Protocol": "File",
                    "SupportsProbing": false,
                    "Sha1": "external-sha1",
                    "Name": "1080p",
                    "ExternalSource": {"owner": "mediatidy"},
                    "MediaStreams": [{
                        "Index": 0,
                        "Type": "Video",
                        "Codec": "old-codec",
                        "DisplayTitle": "old title",
                        "IsDefault": false,
                        "IsForced": false,
                        "ExternalStreamField": "keep"
                    }]
                },
                "Chapters": [{"StartPositionTicks": 1, "Name": "Intro"}],
                "ExternalTopLevel": "keep"
            }]))
            .expect("serialize sidecar"),
        )
        .await
        .expect("write sidecar");

        let mut details = BTreeMap::new();
        details.insert("Width".to_owned(), json!(1920));
        write_media_info_sidecar_with_context(
            &strm,
            &MediaProbeResult {
                container: Some("matroska".to_owned()),
                source_size: Some(1234),
                duration_ticks: Some(125_000_000),
                bitrate: Some(500_000),
                streams: vec![MediaStreamResult {
                    stream_index: 0,
                    stream_type: StreamType::Video,
                    codec: Some("h264".to_owned()),
                    language: None,
                    title: Some("new title".to_owned()),
                    is_default: true,
                    is_forced: false,
                    details,
                }],
            },
            Some(MediaInfoSidecarContext {
                protocol: "Http",
                is_remote: true,
                supports_transcoding: true,
            }),
        )
        .await
        .expect("merge sidecar");

        let written: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&sidecar).await.expect("read sidecar"))
                .expect("parse merged sidecar");
        assert_eq!(written[0]["MediaSourceInfo"]["Container"], "matroska");
        assert_eq!(written[0]["MediaSourceInfo"]["Protocol"], "File");
        assert_eq!(written[0]["MediaSourceInfo"]["SupportsProbing"], false);
        assert_eq!(written[0]["MediaSourceInfo"]["Sha1"], "external-sha1");
        assert_eq!(written[0]["MediaSourceInfo"]["Name"], "1080p");
        assert_eq!(
            written[0]["MediaSourceInfo"]["ExternalSource"]["owner"],
            "mediatidy"
        );
        assert_eq!(
            written[0]["MediaSourceInfo"]["MediaStreams"][0]["Codec"],
            "h264"
        );
        assert_eq!(
            written[0]["MediaSourceInfo"]["MediaStreams"][0]["ExternalStreamField"],
            "keep"
        );
        assert_eq!(written[0]["Chapters"][0]["Name"], "Intro");
        assert_eq!(written[0]["ExternalTopLevel"], "keep");
        assert_eq!(
            tokio::fs::read(&strm).await.expect("read strm"),
            original_strm
        );
    }

    #[tokio::test]
    async fn malformed_existing_sidecar_is_not_replaced() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let strm = temporary.path().join("Movie.strm");
        tokio::fs::write(&strm, b"https://media.example.invalid/movie.mkv")
            .await
            .expect("write strm");
        let sidecar = temporary.path().join("Movie-mediainfo.json");
        let original_sidecar = b"{not-json";
        tokio::fs::write(&sidecar, original_sidecar)
            .await
            .expect("write malformed sidecar");

        let error = write_media_info_sidecar_with_context(
            &strm,
            &MediaProbeResult {
                container: Some("matroska".to_owned()),
                source_size: None,
                duration_ticks: None,
                bitrate: None,
                streams: Vec::new(),
            },
            None,
        )
        .await
        .expect_err("malformed sidecar must be rejected");
        assert!(matches!(error, ProbeError::InvalidOutput(_)));
        assert_eq!(
            tokio::fs::read(&sidecar).await.expect("read sidecar"),
            original_sidecar
        );
    }

    #[test]
    fn sidecar_context_emits_file_source_contract_without_internal_ids() {
        let bytes = serialize_media_info_sidecar_with_context(
            &MediaProbeResult {
                container: Some("matroska".to_owned()),
                source_size: Some(1234),
                duration_ticks: Some(125_000_000),
                bitrate: Some(500_000),
                streams: Vec::new(),
            },
            Some(MediaInfoSidecarContext {
                protocol: "File",
                is_remote: false,
                supports_transcoding: true,
            }),
        )
        .expect("serialize sidecar");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse sidecar");
        let source = &value[0]["MediaSourceInfo"];
        assert_eq!(source["Protocol"], "File");
        assert_eq!(source["Type"], "Default");
        assert_eq!(source["IsRemote"], false);
        assert_eq!(source["SupportsDirectPlay"], true);
        assert_eq!(source["SupportsDirectStream"], true);
        assert_eq!(source["SupportsTranscoding"], true);
        assert!(source.get("Id").is_none());
        assert!(source.get("ItemId").is_none());
        assert!(source.get("Path").is_none());
    }
}

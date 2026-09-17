use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

pub(crate) fn render(
    event_type: &str,
    occurred_at: i64,
    data: &Map<String, Value>,
) -> Map<String, Value> {
    let content = readable_content(event_type, data);
    Map::from_iter([
        ("source".to_owned(), json!("lux")),
        ("title".to_owned(), json!(readable_title(event_type, data))),
        ("content".to_owned(), json!(content.clone())),
        ("body".to_owned(), json!(content)),
        ("timestamp".to_owned(), json!(format_timestamp(occurred_at))),
    ])
}

fn readable_title(event_type: &str, data: &Map<String, Value>) -> String {
    if event_type.starts_with("PLAYBACK_") {
        let user = string_value(data, "userName");
        let item = string_value(data, "itemTitle");
        let action = match event_type {
            "PLAYBACK_STARTED" if data.get("resumed").and_then(Value::as_bool) == Some(true) => {
                "恢复播放"
            }
            "PLAYBACK_STARTED" => "开始播放",
            "PLAYBACK_PAUSED" => "暂停播放",
            "PLAYBACK_PROGRESS" => "播放进度",
            "PLAYBACK_STOPPED" => "停止播放",
            _ => "播放状态",
        };
        let suffix = if item.is_empty() {
            String::new()
        } else {
            format!(" {item}")
        };
        return format!("{user}{action}{suffix}");
    }
    match event_type {
        "MEDIA_ADDED" => "媒体新增".to_owned(),
        "MEDIA_REMOVED" => "媒体移除".to_owned(),
        "SCAN_COMPLETED" => "扫描完成".to_owned(),
        "SCAN_FAILED" => "扫描失败".to_owned(),
        "METADATA_UPDATED" => "元数据更新".to_owned(),
        "JOB_FAILED" => "后台任务失败".to_owned(),
        _ => "Lux 通知".to_owned(),
    }
}

fn readable_content(event_type: &str, data: &Map<String, Value>) -> String {
    if event_type.starts_with("PLAYBACK_") {
        return playback_content(event_type, data);
    }
    let mut lines = Vec::new();
    match event_type {
        "MEDIA_ADDED" => {
            lines.push(format!("新增媒体：{} 个", number_value(data, "addedCount")));
        }
        "MEDIA_REMOVED" => {
            let removed_count =
                number_value(data, "removedCount").max(number_value(data, "deletedFileCount"));
            lines.push(format!("移除媒体：{removed_count} 个"));
        }
        "SCAN_COMPLETED" => lines.push("扫描已完成".to_owned()),
        "SCAN_FAILED" => lines.push("扫描未完成".to_owned()),
        "METADATA_UPDATED" => lines.push("元数据已更新".to_owned()),
        "JOB_FAILED" => lines.push("后台任务执行失败".to_owned()),
        _ => lines.push("收到新的 Lux 事件".to_owned()),
    }
    append_labeled_value(&mut lines, "媒体库", data, "libraryId");
    append_labeled_value(&mut lines, "任务", data, "jobType");
    append_labeled_value(&mut lines, "状态", data, "status");
    append_labeled_value(&mut lines, "错误", data, "errorCode");
    append_labeled_value(&mut lines, "候选", data, "candidateCount");
    lines.join("\n")
}

fn playback_content(event_type: &str, data: &Map<String, Value>) -> String {
    let position = number_value(data, "positionTicks");
    let duration = number_value(data, "durationTicks");
    let percentage = if duration > 0 {
        ((position as f64 / duration as f64) * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };
    let filled = ((percentage / 100.0) * 20.0).floor() as usize;
    let progress = format!(
        "{}{}{:.2}%",
        "●".repeat(filled),
        "○".repeat(20 - filled),
        percentage
    );
    let container = string_value(data, "container").to_ascii_uppercase();
    let method = match string_value(data, "playMethod")
        .to_ascii_lowercase()
        .as_str()
    {
        "directstream" | "direct_stream" => "直接串流",
        "remux" => "重封装",
        "transcode" | "transcoding" => "转码",
        _ => "直接播放",
    };
    let media = if container.is_empty() {
        method.to_owned()
    } else {
        format!("{container} · {method}")
    };
    let mut lines = vec![progress, media];
    if event_type == "PLAYBACK_STOPPED" {
        let size = format_bytes(data.get("size").and_then(Value::as_i64));
        let bitrate = format_bitrate(data.get("bitrate").and_then(Value::as_i64));
        if size.is_some() || bitrate.is_some() {
            lines.push(format!(
                "大小：{} · {}",
                size.unwrap_or_else(|| "未知".to_owned()),
                bitrate.unwrap_or_else(|| "未知".to_owned())
            ));
        }
    }
    let client = string_value(data, "client");
    let device_name = {
        let value = string_value(data, "deviceName");
        if value.is_empty() {
            string_value(data, "deviceType")
        } else {
            value
        }
    };
    let device = if client.eq_ignore_ascii_case(&device_name) {
        client
    } else if !client.is_empty() && !device_name.is_empty() {
        format!("{client} · {device_name}")
    } else if !client.is_empty() {
        client
    } else {
        device_name
    };
    if !device.is_empty() {
        lines.push(format!("设备：{device}"));
    }
    if event_type == "PLAYBACK_STOPPED" {
        let ip = string_value(data, "remoteIp");
        if !ip.is_empty() {
            lines.push(format!("IP：{ip}"));
        }
        let overview = string_value(data, "overview");
        if !overview.is_empty() {
            lines.push(format!("简介：{overview}"));
        }
    }
    lines.join("\n")
}

fn string_value(data: &Map<String, Value>, key: &str) -> String {
    data.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .chars()
        .take(512)
        .collect()
}

fn number_value(data: &Map<String, Value>, key: &str) -> i64 {
    data.get(key)
        .and_then(Value::as_i64)
        .unwrap_or_default()
        .max(0)
}

fn append_labeled_value(
    lines: &mut Vec<String>,
    label: &str,
    data: &Map<String, Value>,
    key: &str,
) {
    let value = string_value(data, key);
    if !value.is_empty() {
        lines.push(format!("{label}：{value}"));
    }
}

fn format_bytes(value: Option<i64>) -> Option<String> {
    let value = value.filter(|value| *value >= 0)? as f64;
    let (value, unit) = if value >= 1_000_000_000.0 {
        (value / 1_000_000_000.0, "GB")
    } else if value >= 1_000_000.0 {
        (value / 1_000_000.0, "MB")
    } else if value >= 1_000.0 {
        (value / 1_000.0, "KB")
    } else {
        (value, "B")
    };
    Some(format_decimal(value, unit))
}

fn format_bitrate(value: Option<i64>) -> Option<String> {
    let value = value.filter(|value| *value >= 0)? as f64 / 1_000_000.0;
    Some(format_decimal(value, "Mbps"))
}

fn format_decimal(value: f64, unit: &str) -> String {
    let text = format!("{value:.2}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned();
    format!("{text}{unit}")
}

fn format_timestamp(timestamp: i64) -> String {
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| timestamp.to_string())
}

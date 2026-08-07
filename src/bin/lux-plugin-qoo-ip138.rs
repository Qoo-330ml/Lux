mod ip_location_plugin;

use std::time::Duration;

use ip_location_plugin::{
    invalid_ip, invalid_response, is_public_ip, read_limited_body, upstream_error,
};
use luxd::application::plugin_protocol::{
    IP_LOCATION_CAPABILITY, IpLocationRpcRequest, IpLocationRpcResult, PluginRpcError,
};
use serde_json::{Value, json};

const PLUGIN_ID: &str = "org.lux.qoo-ip138";
const PLUGIN_NAME: &str = "ip138 IP归属地查询";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    ip_location_plugin::run(|method, params| async move {
        match method.as_str() {
            "plugin.hello" => Ok(json!({
                "id": PLUGIN_ID,
                "name": PLUGIN_NAME,
                "apiVersion": 1,
                "capabilities": [IP_LOCATION_CAPABILITY],
                "supportedItemTypes": []
            })),
            "plugin.health" => Ok(json!({
                "available": true,
                "configured": true
            })),
            "ip.location" => lookup(params).await,
            "plugin.shutdown" => Ok(json!({"accepted": true})),
            _ => Err(PluginRpcError {
                code: "PLUGIN_INVALID_REQUEST".to_owned(),
                message: "unsupported plugin method".to_owned(),
            }),
        }
    })
    .await
}

async fn lookup(params: Value) -> Result<Value, PluginRpcError> {
    let request: IpLocationRpcRequest = serde_json::from_value(params).map_err(|_| invalid_ip())?;
    let ip = is_public_ip(&request.ip).ok_or_else(invalid_ip)?;
    let client = luxd::network::client_builder_from_env()
        .map_err(|_| upstream_error())?
        .timeout(Duration::from_secs(10))
        .user_agent("Lux IP location plugin")
        .build()
        .map_err(|_| upstream_error())?;
    let url = format!("https://www.ipshudi.com/{ip}.htm");
    let response = client
        .get(url)
        .header("dnt", "1")
        .send()
        .await
        .map_err(|_| upstream_error())?;
    if !response.status().is_success() {
        return Err(upstream_error());
    }
    let body = read_limited_body(response, MAX_RESPONSE_BYTES).await?;
    let result = parse_ipshudi_html(&body, ip)?;
    serde_json::to_value(result).map_err(|_| invalid_response())
}

fn parse_ipshudi_html(
    body: &[u8],
    query_ip: std::net::IpAddr,
) -> Result<IpLocationRpcResult, PluginRpcError> {
    let html = std::str::from_utf8(body).map_err(|_| invalid_response())?;
    let table = first_tag_content(html, "table").ok_or_else(invalid_response)?;
    let mut location = None;
    let mut isp = None;
    for row in tag_contents(table, "tr") {
        let cells = row_cells(row);
        if cells.len() < 2 {
            continue;
        }
        let key = clean_html_text(cells[0]);
        let value = clean_html_text(cells[1]);
        if key == "归属地" {
            location = (!value.is_empty()).then_some(value);
        } else if key == "运营商" {
            isp = (!value.is_empty()).then_some(value);
        }
    }
    let location = location.ok_or_else(invalid_response)?;
    Ok(IpLocationRpcResult {
        ip: query_ip.to_string(),
        country: Some(location),
        province: None,
        city: None,
        district: None,
        street: None,
        isp,
        latitude: None,
        longitude: None,
    })
}

fn first_tag_content<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find(&format!("<{tag}"))?;
    let start = lower[open..].find('>')? + open + 1;
    let close_marker = format!("</{tag}>");
    let end = lower[start..].find(&close_marker)? + start;
    Some(&html[start..end])
}

fn tag_contents<'a>(html: &'a str, tag: &str) -> Vec<&'a str> {
    let lower = html.to_ascii_lowercase();
    let open_marker = format!("<{tag}");
    let close_marker = format!("</{tag}>");
    let mut values = Vec::new();
    let mut cursor = 0;
    while let Some(relative_open) = lower[cursor..].find(&open_marker) {
        let open = cursor + relative_open;
        let Some(relative_start) = lower[open..].find('>') else {
            break;
        };
        let start = open + relative_start + 1;
        let Some(relative_end) = lower[start..].find(&close_marker) else {
            break;
        };
        let end = start + relative_end;
        values.push(&html[start..end]);
        cursor = end + close_marker.len();
    }
    values
}

fn row_cells(row: &str) -> Vec<&str> {
    let lower = row.to_ascii_lowercase();
    let mut cells = Vec::new();
    let mut cursor = 0;
    while cursor < row.len() {
        let td = lower[cursor..].find("<td");
        let th = lower[cursor..].find("<th");
        let Some((relative_open, tag)) = (match (td, th) {
            (None, None) => None,
            (Some(td), None) => Some((td, "td")),
            (None, Some(th)) => Some((th, "th")),
            (Some(td), Some(th)) if td < th => Some((td, "td")),
            (Some(th), Some(_td)) => Some((th, "th")),
        }) else {
            break;
        };
        let open = cursor + relative_open;
        let Some(relative_start) = lower[open..].find('>') else {
            break;
        };
        let start = open + relative_start + 1;
        let close_marker = format!("</{tag}>");
        let Some(relative_end) = lower[start..].find(&close_marker) else {
            break;
        };
        let end = start + relative_end;
        cells.push(&row[start..end]);
        cursor = end + close_marker.len();
    }
    cells
}

fn clean_html_text(value: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            character if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("上报纠错", "")
        .replace("Ping", "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::parse_ipshudi_html;

    #[test]
    fn parses_location_and_isp_from_the_provider_table() {
        let html = r#"
            <table><tr><td>归属地</td><td>中国 北京市</td></tr>
            <tr><td>运营商</td><td>中国电信 <a>Ping</a></td></tr></table>
        "#;
        let result = parse_ipshudi_html(html.as_bytes(), "8.8.8.8".parse().expect("valid IP"))
            .expect("provider table should parse");
        assert_eq!(result.country.as_deref(), Some("中国 北京市"));
        assert_eq!(result.isp.as_deref(), Some("中国电信"));
    }
}

use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use reqwest::{Client, Response};

use crate::network::client_builder_from_env_or;

const DIAGNOSTIC_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_TRACE_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkProbeTarget {
    pub id: &'static str,
    pub label: &'static str,
    pub url: &'static str,
}

pub const NETWORK_PROBE_TARGETS: [NetworkProbeTarget; 4] = [
    NetworkProbeTarget {
        id: "tmdb",
        label: "TMDb",
        url: "https://api.themoviedb.org/3/configuration",
    },
    NetworkProbeTarget {
        id: "baidu",
        label: "百度",
        url: "https://www.baidu.com/",
    },
    NetworkProbeTarget {
        id: "google",
        label: "Google",
        url: "https://www.google.com/generate_204",
    },
    NetworkProbeTarget {
        id: "cloudflare",
        label: "Cloudflare",
        url: "https://cloudflare.com/cdn-cgi/trace",
    },
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkProbeResult {
    pub id: &'static str,
    pub label: &'static str,
    pub latency_ms: Option<u64>,
    pub status: Option<u16>,
    pub reachable: bool,
    pub error: Option<&'static str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkDiagnostics {
    pub probes: Vec<NetworkProbeResult>,
    pub egress_ip: Option<String>,
    pub egress_country: Option<String>,
}

pub async fn test_network(proxy_url: Option<&str>) -> NetworkDiagnostics {
    let builder = match client_builder_from_env_or(proxy_url) {
        Ok(builder) => builder,
        Err(_) => return failed_diagnostics("proxy_invalid"),
    };
    let client = match builder
        .timeout(DIAGNOSTIC_TIMEOUT)
        .user_agent("Lux network diagnostics")
        .build()
    {
        Ok(client) => client,
        Err(_) => return failed_diagnostics("client_invalid"),
    };

    let (tmdb, baidu, google, cloudflare) = tokio::join!(
        probe(&client, NETWORK_PROBE_TARGETS[0]),
        probe(&client, NETWORK_PROBE_TARGETS[1]),
        probe(&client, NETWORK_PROBE_TARGETS[2]),
        probe_cloudflare(&client, NETWORK_PROBE_TARGETS[3]),
    );
    let (cloudflare, egress_ip, egress_country) = cloudflare;

    NetworkDiagnostics {
        probes: vec![tmdb, baidu, google, cloudflare],
        egress_ip,
        egress_country,
    }
}

async fn probe(client: &Client, target: NetworkProbeTarget) -> NetworkProbeResult {
    let started = Instant::now();
    match client.get(target.url).send().await {
        Ok(response) => NetworkProbeResult {
            id: target.id,
            label: target.label,
            latency_ms: Some(elapsed_millis(started)),
            status: Some(response.status().as_u16()),
            reachable: true,
            error: None,
        },
        Err(error) => NetworkProbeResult {
            id: target.id,
            label: target.label,
            latency_ms: None,
            status: None,
            reachable: false,
            error: Some(request_error_code(&error)),
        },
    }
}

async fn probe_cloudflare(
    client: &Client,
    target: NetworkProbeTarget,
) -> (NetworkProbeResult, Option<String>, Option<String>) {
    let started = Instant::now();
    let response = match client.get(target.url).send().await {
        Ok(response) => response,
        Err(error) => {
            return (
                NetworkProbeResult {
                    id: target.id,
                    label: target.label,
                    latency_ms: None,
                    status: None,
                    reachable: false,
                    error: Some(request_error_code(&error)),
                },
                None,
                None,
            );
        }
    };
    let status = response.status().as_u16();
    let body = match read_limited_body(response).await {
        Ok(body) => body,
        Err(error) => {
            return (
                NetworkProbeResult {
                    id: target.id,
                    label: target.label,
                    latency_ms: Some(elapsed_millis(started)),
                    status: Some(status),
                    reachable: true,
                    error: Some(error),
                },
                None,
                None,
            );
        }
    };
    let trace = parse_cloudflare_trace(&body);
    (
        NetworkProbeResult {
            id: target.id,
            label: target.label,
            latency_ms: Some(elapsed_millis(started)),
            status: Some(status),
            reachable: true,
            error: trace.as_ref().and_then(|trace| {
                (trace.ip.is_none() || trace.country.is_none()).then_some("trace_unavailable")
            }),
        },
        trace.as_ref().and_then(|trace| trace.ip.clone()),
        trace.and_then(|trace| trace.country),
    )
}

async fn read_limited_body(mut response: Response) -> Result<Vec<u8>, &'static str> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_TRACE_BYTES as u64)
    {
        return Err("response_too_large");
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "response_read_failed")? {
        if body.len().saturating_add(chunk.len()) > MAX_TRACE_BYTES {
            return Err("response_too_large");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CloudflareTrace {
    ip: Option<String>,
    country: Option<String>,
}

fn parse_cloudflare_trace(body: &[u8]) -> Option<CloudflareTrace> {
    let text = std::str::from_utf8(body).ok()?;
    let mut ip = None;
    let mut country = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "ip" => {
                if value.parse::<IpAddr>().is_ok() {
                    ip = Some(value.to_owned());
                }
            }
            "loc" if value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()) => {
                country = Some(value.to_ascii_uppercase());
            }
            _ => {}
        }
    }
    Some(CloudflareTrace { ip, country })
}

fn elapsed_millis(started: Instant) -> u64 {
    match u64::try_from(started.elapsed().as_millis()) {
        Ok(value) => value.max(1),
        Err(_) => u64::MAX,
    }
}

fn request_error_code(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect_failed"
    } else {
        "request_failed"
    }
}

fn failed_diagnostics(error: &'static str) -> NetworkDiagnostics {
    NetworkDiagnostics {
        probes: NETWORK_PROBE_TARGETS
            .into_iter()
            .map(|target| NetworkProbeResult {
                id: target.id,
                label: target.label,
                latency_ms: None,
                status: None,
                reachable: false,
                error: Some(error),
            })
            .collect(),
        egress_ip: None,
        egress_country: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{NETWORK_PROBE_TARGETS, parse_cloudflare_trace};

    #[test]
    fn probe_targets_are_fixed_and_ordered() {
        assert_eq!(
            NETWORK_PROBE_TARGETS
                .iter()
                .map(|target| target.id)
                .collect::<Vec<_>>(),
            vec!["tmdb", "baidu", "google", "cloudflare"]
        );
        assert_eq!(
            NETWORK_PROBE_TARGETS
                .iter()
                .map(|target| target.url)
                .collect::<Vec<_>>(),
            vec![
                "https://api.themoviedb.org/3/configuration",
                "https://www.baidu.com/",
                "https://www.google.com/generate_204",
                "https://cloudflare.com/cdn-cgi/trace",
            ]
        );
    }

    #[test]
    fn cloudflare_trace_accepts_only_valid_ip_and_country_fields() {
        let trace = parse_cloudflare_trace(b"ip=203.0.113.10\nloc=cn\ncolo=SIN\nnot-a-field\n")
            .expect("UTF-8 trace should parse");

        assert_eq!(trace.ip.as_deref(), Some("203.0.113.10"));
        assert_eq!(trace.country.as_deref(), Some("CN"));

        let invalid = parse_cloudflare_trace(b"ip=not-an-ip\nloc=China\n").expect("UTF-8 trace");
        assert_eq!(invalid.ip, None);
        assert_eq!(invalid.country, None);
    }
}

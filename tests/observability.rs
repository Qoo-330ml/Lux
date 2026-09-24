#[cfg(unix)]
mod unix {
    use std::{process::Stdio, time::Duration};

    use serde_json::Value;
    use tokio::{process::Command, time::sleep};

    async fn send_signal(signal: &str, pid: u32) -> Result<(), Box<dyn std::error::Error>> {
        let status = Command::new("kill")
            .args([format!("-{signal}"), pid.to_string()])
            .status()
            .await?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -{signal} {pid} exited with {status}").into())
        }
    }

    async fn wait_for_http(
        child: &mut tokio::process::Child,
        health_url: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let client = reqwest::Client::new();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match client.get(health_url).send().await {
                    Ok(response) if response.status().is_success() => {
                        return Ok::<(), Box<dyn std::error::Error>>(());
                    }
                    Ok(_) | Err(_) => {}
                }
                if let Some(status) = child.try_wait()? {
                    return Err(format!("luxd exited during startup with {status}").into());
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| "timed out waiting for luxd startup")??;
        Ok(())
    }

    async fn read_daily_logs(
        config_dir: &std::path::Path,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut entries = tokio::fs::read_dir(config_dir.join("logs")).await?;
        let mut logs = String::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let name = path.file_name().and_then(|value| value.to_str());
            if name.is_some_and(|value| value.starts_with("lux.") && value.ends_with(".log")) {
                logs.push_str(&tokio::fs::read_to_string(path).await?);
                logs.push('\n');
            }
        }
        if logs.is_empty() {
            return Err("daily Lux log file was not created or was empty".into());
        }
        Ok(logs)
    }

    #[tokio::test]
    async fn request_logs_include_correlation_and_latency_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        let probe_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = probe_listener.local_addr()?;
        drop(probe_listener);
        let mut child = Command::new(env!("CARGO_BIN_EXE_luxd"))
            .env("LUX_HTTP_ADDR", address.to_string())
            .env("LUX_DISCOVERY_BIND_ADDR", "127.0.0.1:0")
            .env("LUX_CONFIG_DIR", &config_dir)
            .env("RUST_LOG", "luxd=debug,tower_http=debug")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let pid = child.id().ok_or("luxd process has no pid")?;
        let health_url = format!("http://{address}/health/live");
        if let Err(error) = wait_for_http(&mut child, &health_url).await {
            let _ = send_signal("KILL", pid).await;
            let _ = child.wait().await;
            return Err(error);
        }

        let client = reqwest::Client::new();
        let response = client.get(&health_url).send().await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let request_id = response
            .headers()
            .get("x-request-id")
            .ok_or("missing response request ID")?
            .to_str()?
            .to_owned();
        assert!(!request_id.is_empty());

        let unauthorized = client
            .get(format!("http://{address}/api/v1/auth/me"))
            .send()
            .await?;
        assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
        let error_request_id = unauthorized
            .headers()
            .get("x-request-id")
            .ok_or("missing error response request ID")?
            .to_str()?
            .to_owned();

        let playback_info = client
            .get(format!(
                "http://{address}/Items/playback-log-item/PlaybackInfo"
            ))
            .query(&[("api_key", "playback-secret")])
            .send()
            .await?;
        assert_eq!(playback_info.status(), reqwest::StatusCode::UNAUTHORIZED);
        let playback_info_request_id = playback_info
            .headers()
            .get("x-request-id")
            .ok_or("missing PlaybackInfo request ID")?
            .to_str()?
            .to_owned();

        let playback_stream = client
            .get(format!("http://{address}/Videos/playback-log-item/stream"))
            .query(&[("api_key", "stream-secret")])
            .send()
            .await?;
        assert_eq!(playback_stream.status(), reqwest::StatusCode::UNAUTHORIZED);
        let playback_stream_request_id = playback_stream
            .headers()
            .get("x-request-id")
            .ok_or("missing playback stream request ID")?
            .to_str()?
            .to_owned();

        send_signal("TERM", pid).await?;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| "luxd did not exit after SIGTERM")??;
        assert!(status.success(), "luxd exited with {}", status);

        let logs = read_daily_logs(&config_dir).await?;
        let response_log = logs
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| {
                value["fields"]["message"] == "finished processing request"
                    && value["span"]["path"] == "/health/live"
                    && value["span"]["requestId"] == request_id
            })
            .ok_or("missing structured request response log")?;
        assert_eq!(response_log["span"]["requestId"], request_id);
        assert!(response_log["span"]["durationMs"].is_number());
        assert_eq!(response_log["span"]["statusCode"], 200);

        let error_log = logs
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| {
                value["fields"]["message"] == "finished processing request"
                    && value["span"]["path"] == "/api/v1/auth/me"
                    && value["span"]["requestId"] == error_request_id
            })
            .ok_or("missing structured error response log")?;
        assert_eq!(error_log["span"]["errorCode"], "AUTHENTICATION_REQUIRED");
        assert_eq!(error_log["span"]["statusCode"], 401);

        let playback_log = logs
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| {
                value["fields"]["event"] == "emby_playback_info"
                    && value["fields"]["request_id"] == playback_info_request_id
            })
            .ok_or("missing structured PlaybackInfo log")?;
        assert_eq!(playback_log["fields"]["status_code"], 401);
        assert_eq!(playback_log["fields"]["item_id_prefix"], "playback");

        let playback_stream_log = logs
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| {
                value["fields"]["event"] == "emby_media_stream_failure"
                    && value["fields"]["request_id"] == playback_stream_request_id
            })
            .ok_or("missing structured playback stream failure log")?;
        assert_eq!(playback_stream_log["fields"]["status_code"], 401);
        assert_eq!(playback_stream_log["fields"]["item_id_prefix"], "playback");
        assert!(!logs.contains("playback-secret"));
        assert!(!logs.contains("stream-secret"));
        assert!(!logs.contains("?api_key"));
        Ok(())
    }

    #[tokio::test]
    async fn request_logs_are_persisted_as_daily_json_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        let probe_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = probe_listener.local_addr()?;
        drop(probe_listener);
        let mut child = Command::new(env!("CARGO_BIN_EXE_luxd"))
            .env("LUX_HTTP_ADDR", address.to_string())
            .env("LUX_DISCOVERY_BIND_ADDR", "127.0.0.1:0")
            .env("LUX_CONFIG_DIR", &config_dir)
            .env("RUST_LOG", "luxd=debug,tower_http=debug")
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()?;
        let pid = child.id().ok_or("luxd process has no pid")?;
        let health_url = format!("http://{address}/health/live");
        if let Err(error) = wait_for_http(&mut child, &health_url).await {
            let _ = send_signal("KILL", pid).await;
            let _ = child.wait().await;
            return Err(error);
        }

        let response = reqwest::get(&health_url).await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        send_signal("TERM", pid).await?;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| "luxd did not exit after SIGTERM")??;
        assert!(status.success(), "luxd exited with {}", status);
        let contents = read_daily_logs(&config_dir).await?;
        let logs = contents
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        assert!(logs.iter().any(|value| {
            value["fields"]["message"] == "luxd listening"
                || value["fields"]["message"] == "database migrations applied"
        }));
        Ok(())
    }
}

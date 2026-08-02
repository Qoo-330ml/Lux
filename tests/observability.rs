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
        tokio::time::timeout(Duration::from_secs(10), async {
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
            .env("LUX_CONFIG_DIR", &config_dir)
            .env("RUST_LOG", "luxd=debug,tower_http=debug")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pid = child.id().ok_or("luxd process has no pid")?;
        let health_url = format!("http://{address}/health/live");
        if let Err(error) = wait_for_http(&mut child, &health_url).await {
            let _ = send_signal("KILL", pid).await;
            return Err(error);
        }

        let response = reqwest::Client::new().get(&health_url).send().await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let request_id = response
            .headers()
            .get("x-request-id")
            .ok_or("missing response request ID")?
            .to_str()?
            .to_owned();
        assert!(!request_id.is_empty());

        send_signal("TERM", pid).await?;
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .map_err(|_| "luxd did not exit after SIGTERM")??;
        assert!(
            output.status.success(),
            "luxd exited with {}",
            output.status
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let logs = format!("{stdout}\n{stderr}");
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
        Ok(())
    }
}

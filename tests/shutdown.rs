#[cfg(unix)]
mod unix {
    use std::{process::Stdio, time::Duration};

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
        sleep(Duration::from_millis(100)).await;
        Ok(())
    }

    #[tokio::test]
    async fn luxd_exits_cleanly_on_sigterm_after_startup() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        let probe_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = probe_listener.local_addr()?;
        drop(probe_listener);
        let mut child = Command::new(env!("CARGO_BIN_EXE_luxd"))
            .env("LUX_HTTP_ADDR", address.to_string())
            .env("LUX_CONFIG_DIR", &config_dir)
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let pid = child.id().ok_or("luxd process has no pid")?;

        let health_url = format!("http://{address}/health/live");
        if let Err(error) = wait_for_http(&mut child, &health_url).await {
            let _ = send_signal("KILL", pid).await;
            return Err(error);
        }

        send_signal("TERM", pid).await?;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| "luxd did not exit after SIGTERM")??;
        assert!(status.success(), "luxd exited with {status}");
        Ok(())
    }
}

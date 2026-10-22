#[cfg(unix)]
mod unix {
    use std::{os::unix::fs::PermissionsExt, process::Stdio, time::Duration};

    use luxd::{
        api::AppState,
        application::{
            libraries::LibraryService,
            probe::{FfprobeRunner, MediaProbeService},
            scanner::LibraryScanner,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };
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
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test client");
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
            .env("LUX_DISCOVERY_BIND_ADDR", "127.0.0.1:0")
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

    #[tokio::test]
    async fn probe_runtime_shutdown_reaps_active_ffprobe_before_return()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let media_root = temp_dir.path().join("Movies");
        let movie_dir = media_root.join("Shutdown Movie (2024)");
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join("Shutdown.Movie.2024.mkv"), b"fixture").await?;
        libraries
            .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;

        let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
        std::fs::write(
            &fake_ffprobe,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$(dirname \"$0\")/pid\"\nexec /bin/sleep 30\n",
        )?;
        let mut permissions = std::fs::metadata(&fake_ffprobe)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_ffprobe, permissions)?;

        let service = MediaProbeService::new(
            database,
            FfprobeRunner::new(fake_ffprobe.clone(), Duration::from_secs(30)),
        );
        let probe_task = tokio::spawn(async move { service.probe_movie_library(library.id).await });
        let pid_file = fake_ffprobe.with_file_name("pid");
        let pid: u32 = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(pid) = tokio::fs::read_to_string(&pid_file).await
                    && let Ok(pid) = pid.trim().parse()
                {
                    break Ok::<u32, Box<dyn std::error::Error>>(pid);
                }
                if probe_task.is_finished() {
                    break Err("probe task finished before fake ffprobe started".into());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "fake ffprobe did not start within twenty seconds")??;

        let pid_text = pid.to_string();
        let is_running = Command::new("kill")
            .args(["-0", pid_text.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await?
            .success();
        assert!(
            is_running,
            "fake ffprobe should still be active before shutdown"
        );

        tokio::time::timeout(
            Duration::from_secs(5),
            AppState::default().shutdown_background_workers_checked(),
        )
        .await
        .map_err(|_| "background worker shutdown did not finish within five seconds")??;

        let reaped = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let alive = Command::new("kill")
                    .args(["-0", pid_text.as_str()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await?
                    .success();
                if !alive {
                    break Ok::<(), Box<dyn std::error::Error>>(());
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| "fake ffprobe remained alive three seconds after shutdown")?;
        reaped?;
        let _ = tokio::time::timeout(Duration::from_secs(3), probe_task)
            .await
            .map_err(|_| "probe task did not return after shutdown")??;
        Ok(())
    }
}

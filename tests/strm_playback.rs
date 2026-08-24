use luxd::application::strm_playback::StrmPlaybackResolver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn resolver_forwards_the_player_user_agent_through_redirects()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let proxy_task = tokio::spawn(async move {
        let mut user_agents = Vec::new();
        for response in [
            "HTTP/1.1 302 Found\r\nLocation: http://media.example.test/cdn.mkv\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\nConnection: close\r\n\r\nX",
        ] {
            let (mut stream, _) = listener.accept().await.expect("proxy should accept");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("proxy request should be readable");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).expect("proxy request should be HTTP");
            user_agents.push(request.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
                    .map(|(_, value)| value.trim().to_owned())
            }));
            stream
                .write_all(response.as_bytes())
                .await
                .expect("proxy response should be writable");
        }
        user_agents
    });

    let resolver = StrmPlaybackResolver::new_with_proxy_for_tests(format!("http://{address}"))?;
    let resolved = resolver
        .resolve(
            "http://media.example.test/302?pickcode=fixture&path=movie.mkv",
            Some("VidHub/9.0 (iPhone; iOS 18.0)"),
        )
        .await?;
    assert_eq!(resolved.as_str(), "http://media.example.test/cdn.mkv");
    assert_eq!(
        proxy_task.await?,
        vec![
            Some("VidHub/9.0 (iPhone; iOS 18.0)".to_owned()),
            Some("VidHub/9.0 (iPhone; iOS 18.0)".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn resolver_returns_the_original_target_for_a_direct_media_response()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let proxy_task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("proxy should accept");
        let mut request = [0_u8; 4096];
        let size = stream
            .read(&mut request)
            .await
            .expect("proxy request should be readable");
        let request = String::from_utf8_lossy(&request[..size]);
        assert!(request.lines().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("user-agent") && value.trim() == "VidHub/9.0"
            })
        }));
        stream
            .write_all(
                b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\nConnection: close\r\n\r\nX",
            )
            .await
            .expect("proxy response should be writable");
    });

    let resolver = StrmPlaybackResolver::new_with_proxy_for_tests(format!("http://{address}"))?;
    let target = "http://media.example.test/video.mkv";
    let resolved = resolver.resolve(target, Some("VidHub/9.0")).await?;
    assert_eq!(resolved.as_str(), target);
    proxy_task.await?;
    Ok(())
}

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    sync::{Arc, Mutex},
    time::Duration,
};

use luxd::application::strm_playback::{
    StrmHostLookupFuture, StrmPlaybackError, StrmPlaybackResolver,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn read_http_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&request).into_owned())
}

/// Non-loopback RFC1918 address of this host, when the machine has one. The direct-path tests
/// bind a real HTTP server on it because the STRM policy allows LAN playback but rejects
/// loopback; hosts without a usable LAN interface skip those tests instead of failing.
fn local_lan_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(address) if address.is_private() && !address.is_loopback() => Some(address),
        IpAddr::V4(_) | IpAddr::V6(_) => None,
    }
}

fn socket(address: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(address), port)
}

/// Serves one 206 answer for a `bytes=0-0` probe and the four requested bytes for a Range read.
async fn start_lan_media_server()
-> Result<(u16, tokio::task::JoinHandle<usize>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let mut accepted = 0;
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(300), listener.accept()).await
        {
            accepted += 1;
            let Ok(request) = read_http_request(&mut stream).await else {
                continue;
            };
            let response = if request.to_ascii_lowercase().contains("range: bytes=0-0") {
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/16\r\nConnection: close\r\n\r\n0"
            } else {
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/16\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n0123"
            };
            let _ = stream.write_all(response.as_bytes()).await;
        }
        accepted
    });
    Ok((port, server))
}

/// Serves range reads over a keep-alive connection and reports how many TCP connections it
/// accepted, so a test can assert that pooled clients reuse connections.
async fn start_keepalive_media_server()
-> Result<(u16, tokio::task::JoinHandle<usize>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let mut accepted = 0;
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(200), listener.accept()).await
        {
            accepted += 1;
            loop {
                let request = match tokio::time::timeout(
                    Duration::from_millis(400),
                    read_http_request(&mut stream),
                )
                .await
                {
                    Ok(Ok(request)) if !request.is_empty() => request,
                    _ => break,
                };
                let (range, body) = if request.to_ascii_lowercase().contains("range: bytes=0-1") {
                    ("0-1", "01")
                } else {
                    ("2-3", "23")
                };
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: 2\r\nContent-Range: bytes {range}/16\r\nContent-Type: video/mp4\r\nConnection: keep-alive\r\n\r\n{body}"
                );
                if stream.write_all(response.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
        accepted
    });
    Ok((port, server))
}

/// A LAN neighbour address whose TCP port refuses connections immediately, if the network has
/// one. Used to prove the pinned address group still falls back after one peer refuses.
fn fast_refusing_neighbor(lan: Ipv4Addr, port: u16) -> Option<SocketAddr> {
    let octets = lan.octets();
    for last in [254_u8, 253, 252, 251, 250] {
        if octets[3] == last {
            continue;
        }
        let candidate = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], last)),
            port,
        );
        if std::net::TcpStream::connect_timeout(&candidate, Duration::from_millis(500))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return Some(candidate);
        }
    }
    None
}

fn lookup_returning(
    addresses: Vec<SocketAddr>,
) -> impl Fn(String, u16) -> StrmHostLookupFuture<'static> + Send + Sync + 'static {
    move |_host, _port| {
        let addresses = addresses.clone();
        Box::pin(async move { Ok(addresses) })
    }
}

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

#[tokio::test]
async fn resolver_rejects_forbidden_literal_and_trailing_dot_targets()
-> Result<(), Box<dyn std::error::Error>> {
    // No proxy listens on this port: every URL below must be rejected by the URL policy before a
    // connection is attempted.
    let resolver = StrmPlaybackResolver::new_with_proxy_for_tests("http://127.0.0.1:9".to_owned())?;
    for target in [
        "http://[::1]:8080/video.mkv",
        "http://[::ffff:127.0.0.1]:8080/video.mkv",
        "http://[::127.0.0.1]:8080/video.mkv",
        "http://[::ffff:169.254.169.254]/latest",
        "http://[fe80::1]/",
        "http://localhost./video.mkv",
        "http://sub.localhost./video.mkv",
        "http://metadata.google.internal./computeMetadata/v1/",
    ] {
        let error = resolver
            .resolve(target, None)
            .await
            .expect_err("forbidden target must be rejected");
        assert!(matches!(error, StrmPlaybackError::InvalidUrl), "{target}");
        let error = resolver
            .fetch_range(target, "bytes=0-3", None)
            .await
            .err()
            .expect("forbidden target must be rejected");
        assert!(matches!(error, StrmPlaybackError::InvalidUrl), "{target}");
    }
    Ok(())
}

#[tokio::test]
async fn resolver_rejects_a_redirect_to_a_forbidden_address()
-> Result<(), Box<dyn std::error::Error>> {
    for location in [
        "http://[::1]:8080/video.mkv",
        "http://169.254.169.254/latest/meta-data",
        "http://localhost./video.mkv",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let proxy = tokio::spawn(async move {
            let mut accepted = 0;
            while let Ok(Ok((mut stream, _))) =
                tokio::time::timeout(Duration::from_millis(300), listener.accept()).await
            {
                accepted += 1;
                let _ = read_http_request(&mut stream).await;
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
            accepted
        });

        let resolver = StrmPlaybackResolver::new_with_proxy_for_tests(format!("http://{address}"))?;
        let error = resolver
            .resolve("http://media.example.test/video.mkv", None)
            .await
            .expect_err("redirect to a forbidden address must be rejected");
        assert!(matches!(error, StrmPlaybackError::InvalidUrl), "{location}");
        assert_eq!(
            proxy.await?,
            1,
            "the forbidden redirect target must not be requested"
        );
    }
    Ok(())
}

#[tokio::test]
async fn resolver_rejects_a_domain_that_resolves_to_a_forbidden_address()
-> Result<(), Box<dyn std::error::Error>> {
    for address in [
        socket(Ipv4Addr::LOCALHOST, 8080),
        socket(Ipv4Addr::new(169, 254, 169, 254), 80),
    ] {
        let resolver =
            StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(vec![address]))?;
        let error = resolver
            .resolve("http://media.example.test/video.mkv", None)
            .await
            .expect_err("a forbidden resolution must be rejected");
        assert!(matches!(error, StrmPlaybackError::InvalidUrl));
    }
    Ok(())
}

#[tokio::test]
async fn resolver_rejects_a_mixed_resolution_even_when_one_address_is_public()
-> Result<(), Box<dyn std::error::Error>> {
    let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(vec![
        SocketAddr::from(([93, 184, 216, 34], 80)),
        socket(Ipv4Addr::LOCALHOST, 80),
    ]))?;
    let error = resolver
        .fetch_range("http://media.example.test/video.mkv", "bytes=0-3", None)
        .await
        .err()
        .expect("a mixed answer must be rejected");
    assert!(matches!(error, StrmPlaybackError::InvalidUrl));
    Ok(())
}

#[tokio::test]
async fn fetch_range_never_contacts_a_forbidden_target() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let resolver = StrmPlaybackResolver::new()?;
    let error = resolver
        .fetch_range(&format!("http://{address}/secret.mkv"), "bytes=0-3", None)
        .await
        .err()
        .expect("loopback must be rejected");
    assert!(matches!(error, StrmPlaybackError::InvalidUrl));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), listener.accept())
            .await
            .is_err(),
        "the forbidden target must not receive a connection"
    );
    Ok(())
}

#[tokio::test]
async fn direct_playback_connects_to_the_pinned_resolved_address()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(lan) = local_lan_address() else {
        eprintln!("skipping DNS pinning test: no non-loopback RFC1918 address available");
        return Ok(());
    };
    let (port, server) = start_lan_media_server().await?;
    let allowed = socket(lan, port);
    // A second answer that would rebind to loopback: only a request that resolves again can see
    // it, and such a request would be rejected (or reach the wrong host).
    let rebound = socket(Ipv4Addr::LOCALHOST, port);
    let calls = Arc::new(Mutex::new(0_usize));
    let lookup = {
        let calls = Arc::clone(&calls);
        move |_host: String, _port: u16| -> StrmHostLookupFuture<'static> {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                let call = {
                    let mut calls = calls.lock().expect("lookup counter");
                    let call = *calls;
                    *calls += 1;
                    call
                };
                if call < 2 {
                    Ok(vec![allowed])
                } else {
                    Ok(vec![rebound])
                }
            })
        }
    };

    let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup)?;
    let target = format!("http://media.example.test:{port}/video.mkv");
    let resolved = resolver.resolve(&target, None).await?;
    assert_eq!(resolved.as_str(), target.as_str());
    let range = resolver.fetch_range(&target, "bytes=0-3", None).await?;
    assert_eq!(range.body, b"0123");
    assert_eq!(
        *calls.lock().expect("lookup counter"),
        2,
        "each request must resolve its host exactly once"
    );
    assert_eq!(server.await?, 2);
    Ok(())
}

#[tokio::test]
async fn direct_redirects_that_resolve_to_loopback_are_rejected()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(lan) = local_lan_address() else {
        eprintln!("skipping DNS redirect test: no non-loopback RFC1918 address available");
        return Ok(());
    };
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();
    let location = format!("http://rebind.example.test:{port}/cdn.mkv");
    let server = tokio::spawn(async move {
        let mut accepted = 0;
        while let Ok(Ok((mut stream, _))) =
            tokio::time::timeout(Duration::from_millis(300), listener.accept()).await
        {
            accepted += 1;
            let _ = read_http_request(&mut stream).await;
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
        accepted
    });
    let allowed = socket(lan, port);
    let rebound = socket(Ipv4Addr::LOCALHOST, port);
    let lookup = move |host: String, _port: u16| -> StrmHostLookupFuture<'static> {
        Box::pin(async move {
            if host.starts_with("rebind.") {
                Ok(vec![rebound])
            } else {
                Ok(vec![allowed])
            }
        })
    };

    let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup)?;
    let error = resolver
        .resolve(&format!("http://media.example.test:{port}/video.mkv"), None)
        .await
        .expect_err("a redirect that resolves to loopback must be rejected");
    assert!(matches!(error, StrmPlaybackError::InvalidUrl));
    assert_eq!(
        server.await?,
        1,
        "the forbidden redirect target must not be requested"
    );
    Ok(())
}

#[tokio::test]
async fn direct_playback_falls_back_between_pinned_addresses()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(lan) = local_lan_address() else {
        eprintln!(
            "skipping multi-address fallback test: no non-loopback RFC1918 address available"
        );
        return Ok(());
    };
    let (port, server) = start_lan_media_server().await?;
    let allowed = socket(lan, port);
    let Some(refusing) = fast_refusing_neighbor(lan, port) else {
        eprintln!(
            "skipping multi-address fallback test: no LAN neighbour refuses connections quickly"
        );
        return Ok(());
    };
    // The first checked address refuses immediately; the request must still reach the second
    // pinned address instead of failing with a 502.
    let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(vec![
        refusing, allowed,
    ]))?;
    let target = format!("http://media.example.test:{port}/video.mkv");
    let range = resolver.fetch_range(&target, "bytes=0-3", None).await?;
    assert_eq!(range.body, b"0123");
    assert_eq!(server.await?, 1);
    Ok(())
}

#[tokio::test]
async fn direct_playback_reuses_one_connection_for_range_seeks()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(lan) = local_lan_address() else {
        eprintln!("skipping client reuse test: no non-loopback RFC1918 address available");
        return Ok(());
    };
    let (port, server) = start_keepalive_media_server().await?;
    let allowed = socket(lan, port);
    let calls = Arc::new(Mutex::new(0_usize));
    let lookup = {
        let calls = Arc::clone(&calls);
        move |_host: String, _port: u16| -> StrmHostLookupFuture<'static> {
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                *calls.lock().expect("lookup counter") += 1;
                Ok(vec![allowed])
            })
        }
    };

    let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup)?;
    let target = format!("http://media.example.test:{port}/video.mkv");
    let first = resolver.fetch_range(&target, "bytes=0-1", None).await?;
    assert_eq!(first.body, b"01");
    let second = resolver.fetch_range(&target, "bytes=2-3", None).await?;
    assert_eq!(second.body, b"23");
    assert_eq!(
        *calls.lock().expect("lookup counter"),
        2,
        "each request must revalidate the host before reusing a pooled client"
    );
    assert_eq!(
        server.await?,
        1,
        "both range seeks must reuse the pooled client connection"
    );
    Ok(())
}

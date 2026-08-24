use std::io;

use luxd::application::strm_playback::StrmPlaybackResolver;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn accept_and_respond(listener: &TcpListener, response: &str) -> io::Result<()> {
    let (mut stream, _) = listener.accept().await?;
    let mut request = [0_u8; 4096];
    let _ = stream.read(&mut request).await?;
    stream.write_all(response.as_bytes()).await
}

#[tokio::test]
async fn resolves_any_http_redirect_target_without_endpoint_specific_logic()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_address = listener.local_addr()?;
    let responses = vec![
        "HTTP/1.1 302 Found\r\nLocation: /resolved/media.mkv\r\nContent-Length: 0\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Type: video/x-matroska\r\n\r\nx"
            .to_owned(),
    ];
    let server = tokio::spawn(async move {
        for response in responses {
            accept_and_respond(&listener, &response).await?;
        }
        Ok::<(), io::Error>(())
    });

    let resolver = StrmPlaybackResolver::new(Some(format!("http://{proxy_address}")))?;
    let resolved = resolver
        .resolve("http://media.example.test/custom/resolve?id=1")
        .await?;
    server.await??;

    assert_eq!(
        resolved.as_str(),
        "http://media.example.test/resolved/media.mkv"
    );
    Ok(())
}

#[tokio::test]
async fn returns_original_url_when_it_is_already_media_content()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_address = listener.local_addr()?;
    let response = "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\nContent-Type: video/x-matroska\r\n\r\nx";
    let server = tokio::spawn(async move { accept_and_respond(&listener, response).await });

    let resolver = StrmPlaybackResolver::new(Some(format!("http://{proxy_address}")))?;
    let target = "http://media.example.test/media/movie.mkv";
    let resolved = resolver.resolve(target).await?;
    server.await??;

    assert_eq!(resolved.as_str(), target);
    Ok(())
}

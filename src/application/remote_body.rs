#[derive(Debug, Eq, PartialEq)]
pub(crate) enum LimitedBodyError {
    Download(String),
    TooLarge { observed: u64, max: u64 },
}

pub(crate) async fn read_response_body_limited(
    mut response: reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, LimitedBodyError> {
    let declared_length = response.content_length();
    if let Some(length) = declared_length
        && length > max_bytes
    {
        return Err(LimitedBodyError::TooLarge {
            observed: length,
            max: max_bytes,
        });
    }

    let initial_capacity = declared_length
        .map(|length| length.min(max_bytes))
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or_default();
    let mut body = Vec::with_capacity(initial_capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| LimitedBodyError::Download(error.to_string()))?
    {
        let observed = u64::try_from(body.len())
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if observed > max_bytes {
            return Err(LimitedBodyError::TooLarge {
                observed,
                max: max_bytes,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{LimitedBodyError, read_response_body_limited};
    use reqwest::header::CONTENT_TYPE;
    use tokio::{
        io::AsyncWriteExt,
        net::TcpListener,
        time::{Duration, timeout},
    };

    #[tokio::test]
    async fn stops_reading_a_chunked_response_as_soon_as_it_exceeds_the_limit() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let address = listener
            .local_addr()
            .expect("test listener should have an address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("client should connect");
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("response headers should be written");
            socket
                .write_all(b"9\r\n123456789\r\n")
                .await
                .expect("response chunk should be written");
            tokio::time::sleep(Duration::from_secs(1)).await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .expect("test client should build");
        let response = client
            .get(format!("http://{address}/image"))
            .header(CONTENT_TYPE, "image/png")
            .send()
            .await
            .expect("response headers should arrive");
        let result = timeout(
            Duration::from_millis(500),
            read_response_body_limited(response, 8),
        )
        .await
        .expect("limited body read should stop before the client timeout");

        assert!(matches!(
            result,
            Err(LimitedBodyError::TooLarge {
                observed: 9,
                max: 8
            })
        ));
        server.abort();
    }
}

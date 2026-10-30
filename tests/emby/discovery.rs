use std::{net::SocketAddr, time::Duration};

use luxd::discovery::{
    DISCOVERY_PORT, DiscoveryConfig, DiscoveryConfigError, DiscoveryResponse, DiscoveryService,
};
use serde_json::from_slice;
use tokio::{net::UdpSocket, sync::watch, time::timeout};

#[tokio::test]
async fn discovery_uses_emby_request_and_returns_configured_address()
-> Result<(), Box<dyn std::error::Error>> {
    let config = DiscoveryConfig::new(
        "127.0.0.1:0".parse()?,
        "127.0.0.1:8097".parse()?,
        Some("https://media.example.test/lux".to_owned()),
    )?;
    let service = DiscoveryService::bind(config, "server-123", "Lux Test").await?;
    assert_eq!(DISCOVERY_PORT, 7359);

    let listen_addr = service.local_addr()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(service.run(shutdown_rx));
    let client = UdpSocket::bind("127.0.0.1:0").await?;

    client.send_to(b"WHO IS EMBYSERVER?", listen_addr).await?;
    let mut buffer = [0_u8; 8192];
    let (length, _) = timeout(Duration::from_secs(1), client.recv_from(&mut buffer)).await??;
    let response: DiscoveryResponse = from_slice(&buffer[..length])?;
    assert_eq!(response.address, "https://media.example.test/lux");
    assert_eq!(response.id, "server-123");
    assert_eq!(response.name, "Lux Test");

    client
        .send_to(b"not a discovery request", listen_addr)
        .await?;
    assert!(
        timeout(Duration::from_millis(100), client.recv_from(&mut buffer))
            .await
            .is_err()
    );

    shutdown_tx.send(true)?;
    task.await??;
    Ok(())
}

#[tokio::test]
async fn discovery_accepts_utf16le_and_derives_local_http_address()
-> Result<(), Box<dyn std::error::Error>> {
    let config = DiscoveryConfig::new("127.0.0.1:0".parse()?, "127.0.0.1:8097".parse()?, None)?;
    let service = DiscoveryService::bind(config, "server-456", "Lux").await?;
    let listen_addr = service.local_addr()?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(service.run(shutdown_rx));
    let client = UdpSocket::bind("127.0.0.1:0").await?;

    let request: Vec<u8> = "who is EmbyServer?"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    client.send_to(&request, listen_addr).await?;
    let mut buffer = [0_u8; 8192];
    let (length, _) = timeout(Duration::from_secs(1), client.recv_from(&mut buffer)).await??;
    let response_units: Vec<u16> = buffer[..length]
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let response_text = String::from_utf16(&response_units)?;
    let response: DiscoveryResponse = serde_json::from_str(&response_text)?;
    assert_eq!(response.address, "http://127.0.0.1:8097");

    shutdown_tx.send(true)?;
    task.await??;
    Ok(())
}

#[test]
fn discovery_rejects_credentials_query_and_fragment_in_advertised_url()
-> Result<(), Box<dyn std::error::Error>> {
    let bind_addr: SocketAddr = "127.0.0.1:0".parse()?;
    let http_addr: SocketAddr = "127.0.0.1:8097".parse()?;
    for address in [
        "https://user:password@media.example.test",
        "https://media.example.test?token=secret",
        "https://media.example.test/#fragment",
        "ftp://media.example.test",
    ] {
        assert!(matches!(
            DiscoveryConfig::new(bind_addr, http_addr, Some(address.to_owned())),
            Err(DiscoveryConfigError::InvalidAdvertiseUrl)
        ));
    }
    Ok(())
}

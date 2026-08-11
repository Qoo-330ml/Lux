use luxd::application::plugins::validate_strm_resolver_url;

#[test]
fn accepts_a_client_playable_http_url() {
    assert!(validate_strm_resolver_url(
        "https://media.example.test/video.mkv?signature=fixture"
    ));
}

#[test]
fn rejects_unsafe_or_malformed_resolver_urls() {
    for value in [
        "http://user:password@example.test/video.mkv",
        "https://media.example.test/video.mkv#fragment",
        "file:///media/video.mkv",
        "https:///missing-host/video.mkv",
        " https://media.example.test/video.mkv",
        "https://media.example.test/video\n.mkv",
    ] {
        assert!(!validate_strm_resolver_url(value), "URL should be rejected: {value:?}");
    }
}

#[test]
fn rejects_an_overlong_resolver_url() {
    let value = format!("https://media.example.test/{}", "x".repeat(8 * 1024));

    assert!(!validate_strm_resolver_url(&value));
}

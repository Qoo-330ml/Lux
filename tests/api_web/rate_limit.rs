use luxd::security::LoginRateLimiter;

#[tokio::test]
async fn login_rate_limiter_blocks_after_repeated_failures_and_resets_on_success() {
    let limiter = LoginRateLimiter::default();
    for _ in 0..5 {
        assert!(limiter.try_acquire("viewer@local").await);
    }
    assert!(!limiter.try_acquire("viewer@local").await);
    limiter.record_success("viewer@local").await;
    assert!(limiter.try_acquire("viewer@local").await);
}

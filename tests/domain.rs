use std::{
    any::TypeId,
    time::{Duration, UNIX_EPOCH},
};

use luxd::{
    api::lux::{ApiError, ApiErrorCode, ApiErrorEnvelope},
    domain::{
        ids::{ItemId, LibraryId, UserId},
        time::{TICKS_PER_SECOND, UtcTime, duration_to_ticks, ticks_to_duration},
    },
};

#[test]
fn domain_ids_are_distinct_uuidv7_newtypes() -> Result<(), Box<dyn std::error::Error>> {
    let user_id = UserId::new();
    let item_id = ItemId::new();

    assert_ne!(TypeId::of::<UserId>(), TypeId::of::<ItemId>());
    assert_ne!(TypeId::of::<LibraryId>(), TypeId::of::<UserId>());
    assert_eq!(user_id, user_id.to_string().parse()?);
    assert_eq!(user_id.uuid().get_version_num(), 7);
    assert_ne!(user_id.to_string(), item_id.to_string());
    Ok(())
}

#[test]
fn utc_time_and_emby_ticks_have_explicit_boundaries() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        duration_to_ticks(Duration::from_secs(1))?,
        TICKS_PER_SECOND as i64
    );
    assert_eq!(duration_to_ticks(Duration::from_millis(1))?, 10_000);
    assert_eq!(ticks_to_duration(0)?, Duration::ZERO);
    assert!(ticks_to_duration(-1).is_err());
    assert_eq!(
        ticks_to_duration(TICKS_PER_SECOND as i64)?,
        Duration::from_secs(1)
    );
    assert!(duration_to_ticks(Duration::MAX).is_err());
    assert_eq!(
        UtcTime::from_system_time(UNIX_EPOCH).duration_since_epoch()?,
        Duration::ZERO
    );
    assert!(
        UtcTime::from_system_time(UNIX_EPOCH - Duration::from_secs(1))
            .duration_since_epoch()
            .is_err()
    );
    Ok(())
}

#[test]
fn lux_api_errors_have_stable_codes_and_field_names() -> Result<(), Box<dyn std::error::Error>> {
    let body = ApiErrorEnvelope::from(ApiError::new(
        ApiErrorCode::LibraryPathNotWritable,
        "媒体目录不可写",
        "request-123",
    ));
    let json = serde_json::to_value(body)?;

    assert_eq!(json["error"]["code"], "LIBRARY_PATH_NOT_WRITABLE");
    assert_eq!(json["error"]["message"], "媒体目录不可写");
    assert_eq!(json["error"]["requestId"], "request-123");
    Ok(())
}

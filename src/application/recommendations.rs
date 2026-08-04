use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const RECOMMENDATION_CANDIDATE_POOL: i64 = 60;

pub(crate) fn current_day_bucket() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0)
}

pub(crate) fn daily_recommendation_items<T>(
    mut items: Vec<T>,
    user_id: &str,
    day_bucket: u64,
    limit: usize,
) -> Vec<T> {
    if items.len() > 1 {
        let offset = daily_rotation_offset(user_id, day_bucket, items.len());
        items.rotate_left(offset);
    }
    items.truncate(limit);
    items
}

fn daily_rotation_offset(user_id: &str, day_bucket: u64, item_count: usize) -> usize {
    let user_hash = user_id.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    (u64::from(user_hash).wrapping_add(day_bucket) % item_count as u64) as usize
}

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const RECOMMENDATION_CANDIDATE_POOL: i64 = 60;

pub(crate) fn current_recommendation_batch_key() -> i64 {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default();
    crate::storage::recommendation_batch_key_at(timestamp)
}

pub(crate) fn recommendation_library_scope_key(library_ids: &[String]) -> String {
    let mut library_ids = library_ids.to_vec();
    library_ids.sort_unstable();
    library_ids.dedup();
    library_ids.join("\u{001f}")
}

pub(crate) fn daily_recommendation_items<T>(
    mut items: Vec<T>,
    user_id: &str,
    batch_key: i64,
    limit: usize,
) -> Vec<T> {
    if items.len() > 1 {
        let offset = daily_rotation_offset(user_id, batch_key, items.len());
        items.rotate_left(offset);
    }
    items.truncate(limit);
    items
}

fn daily_rotation_offset(user_id: &str, batch_key: i64, item_count: usize) -> usize {
    let user_hash = user_id.bytes().fold(2_166_136_261_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
    });
    (u64::from(user_hash).wrapping_add(batch_key as u64) % item_count as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::recommendation_library_scope_key;

    #[test]
    fn recommendation_batch_changes_at_utc_two_am() {
        let day = 20_i64;
        let two_am = day * 86_400 + 2 * 3_600;

        assert_eq!(
            crate::storage::recommendation_batch_key_at(two_am - 1),
            day - 1
        );
        assert_eq!(crate::storage::recommendation_batch_key_at(two_am), day);
    }

    #[test]
    fn recommendation_library_scope_key_is_order_independent() {
        assert_eq!(
            recommendation_library_scope_key(&["library-b".to_owned(), "library-a".to_owned()]),
            "library-a\u{001f}library-b"
        );
    }
}

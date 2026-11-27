//! Daily recommendation selection.
//!
//! The scoring query only returns `MOVIE` and `SERIES` rows
//! (`item_type IN ('MOVIE', 'SERIES')`), so candidate rows never carry a parent series: seasons
//! and episodes are not recommended and this module needs no series grouping.

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

/// Candidate rows fetched from the scoring query for one daily batch.
///
/// A pool of 100 keeps seven days of non-repeating seven-slot carousels well supplied while still
/// letting the ranking tail surface (`0`-based rank 99 draws at ~2.5% of rank 0's weight).
pub(crate) const RECOMMENDATION_CANDIDATE_POOL: i64 = 100;

/// Leading home-carousel slots selected by [`daily_recommendation_items`].
///
/// Home requests today ask for seven recommendations; the stored batch always starts with this
/// curated prefix so any consumer with a limit up to seven receives the diversified picks.
pub(crate) const RECOMMENDATION_FEATURED_LIMIT: usize = 7;

/// Number of preceding daily batches retained and consulted to avoid repeating recently featured
/// carousel items before fresher candidates have had a turn.
pub(crate) const RECOMMENDATION_COOLDOWN_BATCHES: i64 = 7;

/// Exponent of the rank-derived sampling weight.
///
/// The SQL query ranks by a weighted score that Rust cannot see, so the selection works from the
/// rank position instead: `weight = 1 / (rank + 1)^0.8`, where `rank` is the 0-based position in
/// the score-ordered pool. `0.8` is a sublinear Zipf-like profile: it decays fast enough that the
/// head dominates (0-based rank 2 keeps ~42% of the top weight, rank 6 ~24%) but slowly enough
/// that the tail stays reachable, which is what makes the carousel rotate instead of freezing on
/// the top few rows.
const RANK_WEIGHT_EXPONENT: f64 = 0.8;

/// Lower bound for a rank weight so the logarithmic sampling key stays finite.
const MIN_RANK_WEIGHT: f64 = 1e-6;

/// A single `item_type` may occupy at most this many featured slots. The pool only holds movies
/// and series, so the cap leaves room for both kinds in a seven-slot carousel; it is relaxed when
/// the pool cannot satisfy it.
const MAX_FEATURED_PER_ITEM_TYPE: usize = 4;

/// Items carrying 180-day playback engagement (`recent_playback_score > 0`) may occupy at most
/// this many featured slots, relaxed only when fresher candidates cannot fill the carousel.
const MAX_FEATURED_RECENT_PLAYBACK: usize = 5;

/// The catalog fields the daily selection needs. Catalog rows are ranked by the SQL query; this
/// trait exposes only the stable identity and the type dimension of a candidate.
///
/// Candidates are only `MOVIE` or `SERIES` rows: seasons and episodes do not take part in
/// recommendations, so a candidate is never a child of another candidate.
pub(crate) trait RecommendationCandidate {
    /// Stable identity used to seed the deterministic draw. It must not change across restarts.
    fn candidate_key(&self) -> &str;

    /// `item_type` (for example `MOVIE` or `SERIES`) used by the per-type cap.
    fn candidate_kind(&self) -> &str;

    /// True when the item has recent 180-day playback activity (`recent_playback_score > 0`).
    fn has_recent_playback(&self) -> bool {
        false
    }
}

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

/// Builds the stored daily batch from score-ordered candidates.
///
/// The result starts with the curated prefix (anchors plus the weighted draw, in selection
/// order) and continues with the remaining candidates in their original score order, so callers
/// that read the whole batch still receive a complete, meaningful list. A caller asking for at
/// most [`RECOMMENDATION_FEATURED_LIMIT`] items therefore gets the diversified picks.
/// Convenience wrapper without cooldown history, used by unit tests that examine single-batch
/// weighting and cap behavior in isolation.
#[cfg(test)]
pub(crate) fn daily_recommendation_items<T: RecommendationCandidate>(
    items: Vec<T>,
    user_id: &str,
    batch_key: i64,
    featured_limit: usize,
    batch_limit: usize,
) -> Vec<T> {
    daily_recommendation_items_with_cooldown(
        items,
        user_id,
        batch_key,
        featured_limit,
        batch_limit,
        &[],
    )
}

pub(crate) fn daily_recommendation_items_with_cooldown<T: RecommendationCandidate>(
    items: Vec<T>,
    user_id: &str,
    batch_key: i64,
    featured_limit: usize,
    batch_limit: usize,
    recent_batches: &[(i64, Vec<String>)],
) -> Vec<T> {
    let batch_limit = batch_limit.min(items.len());
    if batch_limit == 0 {
        return Vec::new();
    }
    let featured_limit = featured_limit.min(batch_limit);
    let curated =
        curated_prefix_indices(&items, user_id, batch_key, featured_limit, recent_batches);

    let mut in_prefix = vec![false; items.len()];
    for index in &curated {
        if let Some(slot) = in_prefix.get_mut(*index) {
            *slot = true;
        }
    }

    let mut result = Vec::with_capacity(batch_limit);
    let mut slots = items.into_iter().map(Some).collect::<Vec<_>>();
    for index in curated {
        if result.len() >= batch_limit {
            break;
        }
        if let Some(item) = slots.get_mut(index).and_then(Option::take) {
            result.push(item);
        }
    }
    for (index, slot) in slots.iter_mut().enumerate() {
        if result.len() >= batch_limit {
            break;
        }
        if in_prefix.get(index).copied().unwrap_or_default() {
            continue;
        }
        if let Some(item) = slot.take() {
            result.push(item);
        }
    }
    result
}

/// Picks the curated prefix via weighted sampling without replacement and recent-batch cooldown.
///
/// Candidates not featured in the preceding [`RECOMMENDATION_COOLDOWN_BATCHES`] batches are
/// considered first; when a smaller library runs out of fresh items, older cooldown batches are
/// revisited before yesterday's batch so consecutive days still avoid back-to-back repeats.
fn curated_prefix_indices<T: RecommendationCandidate>(
    items: &[T],
    user_id: &str,
    batch_key: i64,
    featured_limit: usize,
    recent_batches: &[(i64, Vec<String>)],
) -> Vec<usize> {
    let featured_limit = featured_limit.min(items.len());
    if featured_limit == 0 {
        return Vec::new();
    }

    let mut recent_age: HashMap<&str, i64> = HashMap::new();
    for (past_batch_key, past_ids) in recent_batches {
        if *past_batch_key >= batch_key {
            continue;
        }
        let age = batch_key.saturating_sub(*past_batch_key);
        if age > RECOMMENDATION_COOLDOWN_BATCHES {
            continue;
        }
        for item_id in past_ids.iter().take(featured_limit) {
            recent_age
                .entry(item_id.as_str())
                .and_modify(|existing| *existing = (*existing).min(age))
                .or_insert(age);
        }
    }

    let mut drawn = (0..items.len())
        .filter_map(|rank| {
            let item = items.get(rank)?;
            let weight = rank_weight(rank);
            let unit = deterministic_unit_interval(user_id, batch_key, item.candidate_key());
            Some((weighted_sampling_key(weight, unit), rank))
        })
        .collect::<Vec<_>>();
    // Efraimidis–Spirakis keeps the largest `ln(u) / weight` keys; the index tie-break makes the
    // order total so equal keys cannot depend on the sort implementation.
    drawn.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));

    let mut fresh = Vec::with_capacity(drawn.len());
    let mut older_cooldown = Vec::new();
    let mut yesterday_cooldown = Vec::new();
    for (sampling_key, rank) in drawn {
        let Some(item) = items.get(rank) else {
            continue;
        };
        match recent_age.get(item.candidate_key()).copied() {
            None => fresh.push(rank),
            Some(1) => yesterday_cooldown.push(rank),
            Some(age) => older_cooldown.push((age, sampling_key, rank)),
        }
    }
    older_cooldown.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(right.1.total_cmp(&left.1))
            .then(left.2.cmp(&right.2))
    });
    let older_cooldown_ranks = older_cooldown
        .into_iter()
        .map(|(_, _, rank)| rank)
        .collect::<Vec<_>>();

    let mut selected = Vec::with_capacity(featured_limit);
    let mut chosen = vec![false; items.len()];
    let mut type_counts: HashMap<&str, usize> = HashMap::new();
    let mut recent_playback_count = 0_usize;

    for tier in [&fresh, &older_cooldown_ranks, &yesterday_cooldown] {
        if selected.len() >= featured_limit {
            break;
        }
        for relaxation in [
            Relaxation::Strict,
            Relaxation::TypeCapRelaxed,
            Relaxation::AllCapsRelaxed,
        ] {
            if selected.len() >= featured_limit {
                break;
            }
            for index in tier {
                if selected.len() >= featured_limit {
                    break;
                }
                if chosen.get(*index).copied().unwrap_or_default() {
                    continue;
                }
                let Some(item) = items.get(*index) else {
                    continue;
                };
                let type_count = type_counts
                    .get(item.candidate_kind())
                    .copied()
                    .unwrap_or_default();
                let type_allowed =
                    relaxation != Relaxation::Strict || type_count < MAX_FEATURED_PER_ITEM_TYPE;
                if !type_allowed {
                    continue;
                }
                let playback_allowed = relaxation == Relaxation::AllCapsRelaxed
                    || !item.has_recent_playback()
                    || recent_playback_count < MAX_FEATURED_RECENT_PLAYBACK;
                if !playback_allowed {
                    continue;
                }
                if let Some(slot) = chosen.get_mut(*index) {
                    *slot = true;
                }
                *type_counts.entry(item.candidate_kind()).or_default() += 1;
                if item.has_recent_playback() {
                    recent_playback_count += 1;
                }
                selected.push(*index);
            }
        }
    }

    selected
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relaxation {
    /// Both the per-type cap and the recent-playback cap apply.
    Strict,
    /// The per-type cap is ignored while the recent-playback cap still applies.
    TypeCapRelaxed,
    /// All diversity caps are relaxed so a thin tier still fills every slot.
    AllCapsRelaxed,
}

/// Rank-derived sampling weight: `1 / (rank + 1)^[`RANK_WEIGHT_EXPONENT`]`.
fn rank_weight(rank: usize) -> f64 {
    let denominator = (rank as f64 + 1.0).powf(RANK_WEIGHT_EXPONENT);
    if denominator.is_finite() && denominator > 0.0 {
        (1.0 / denominator).clamp(MIN_RANK_WEIGHT, 1.0)
    } else {
        MIN_RANK_WEIGHT
    }
}

/// Weighted-sampling key `ln(u) / weight`, which orders identically to `u^(1 / weight)` but
/// avoids the exponentiation. Degenerate inputs sort last instead of producing `NaN` keys.
fn weighted_sampling_key(weight: f64, unit: f64) -> f64 {
    let weight_usable = weight.is_finite() && weight > 0.0;
    let unit_usable = unit.is_finite() && unit > 0.0 && unit < 1.0;
    if !weight_usable || !unit_usable {
        return f64::NEG_INFINITY;
    }
    let key = unit.ln() / weight;
    if key.is_finite() {
        key
    } else {
        f64::NEG_INFINITY
    }
}

/// Deterministic draw in `(0, 1)` for one `(user, batch, candidate)` triple.
///
/// The seed is a hand-rolled FNV-1a / SplitMix64 chain on purpose: it must stay identical across
/// processes and restarts, which rules out `RandomState` and keeps the crate dependency-free.
fn deterministic_unit_interval(user_id: &str, batch_key: i64, item_key: &str) -> f64 {
    let mut state = fnv1a64(user_id.as_bytes());
    state = splitmix64(state ^ fnv1a64(item_key.as_bytes()));
    state = splitmix64(state ^ (batch_key as u64));
    unit_interval_from_bits(splitmix64(state))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn splitmix64(state: u64) -> u64 {
    let mixed = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    mixed ^ (mixed >> 31)
}

fn unit_interval_from_bits(bits: u64) -> f64 {
    // 53 mantissa bits, offset by half a step so the value stays strictly inside `(0, 1)` and
    // `ln(u)` is always finite.
    const DENOMINATOR: f64 = (1_u64 << 53) as f64;
    (((bits >> 11) as f64) + 0.5) / DENOMINATOR
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        RECOMMENDATION_CANDIDATE_POOL, RECOMMENDATION_COOLDOWN_BATCHES,
        RECOMMENDATION_FEATURED_LIMIT, RecommendationCandidate, daily_recommendation_items,
        daily_recommendation_items_with_cooldown, deterministic_unit_interval, rank_weight,
        recommendation_library_scope_key, weighted_sampling_key,
    };

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

    #[derive(Clone, Debug, PartialEq)]
    struct TestItem {
        id: String,
        kind: &'static str,
        recent_playback: bool,
    }

    impl RecommendationCandidate for TestItem {
        fn candidate_key(&self) -> &str {
            &self.id
        }

        fn candidate_kind(&self) -> &str {
            self.kind
        }

        fn has_recent_playback(&self) -> bool {
            self.recent_playback
        }
    }

    fn movie(index: usize) -> TestItem {
        TestItem {
            id: format!("item-{index:03}"),
            kind: "MOVIE",
            recent_playback: false,
        }
    }

    fn series_item(index: usize) -> TestItem {
        TestItem {
            id: format!("item-{index:03}"),
            kind: "SERIES",
            recent_playback: false,
        }
    }

    fn recent_playback_movie(index: usize) -> TestItem {
        TestItem {
            id: format!("item-{index:03}"),
            kind: "MOVIE",
            recent_playback: true,
        }
    }

    /// Alternating kinds keep the per-type cap satisfiable, so the statistics below measure the
    /// ranking weights and not the relaxation pass.
    fn mixed_items(count: usize) -> Vec<TestItem> {
        (0..count)
            .map(|index| {
                if index % 2 == 0 {
                    movie(index)
                } else {
                    series_item(index)
                }
            })
            .collect()
    }

    fn ids(items: &[TestItem]) -> Vec<String> {
        items.iter().map(|item| item.id.clone()).collect()
    }

    #[test]
    fn daily_recommendations_are_stable_within_one_batch_and_change_between_batches() {
        let first = daily_recommendation_items(
            mixed_items(40),
            "user-1",
            20,
            RECOMMENDATION_FEATURED_LIMIT,
            40,
        );
        let same_batch = daily_recommendation_items(
            mixed_items(40),
            "user-1",
            20,
            RECOMMENDATION_FEATURED_LIMIT,
            40,
        );
        assert_eq!(first, same_batch);

        let distinct_batches = (20..40)
            .map(|batch_key| {
                ids(&daily_recommendation_items(
                    mixed_items(40),
                    "user-1",
                    batch_key,
                    RECOMMENDATION_FEATURED_LIMIT,
                    40,
                ))
            })
            .collect::<HashSet<_>>();
        assert!(
            distinct_batches.len() > 1,
            "a new batch must change the featured recommendation"
        );
    }

    #[test]
    fn candidate_pool_depth_is_one_hundred() {
        assert_eq!(RECOMMENDATION_CANDIDATE_POOL, 100);
        assert_eq!(RECOMMENDATION_COOLDOWN_BATCHES, 7);
    }

    #[test]
    fn daily_recommendations_rotate_leading_slots_instead_of_pinning_anchors() {
        let mut first_slots = HashSet::new();
        let mut second_slots = HashSet::new();
        for batch_key in 0..30 {
            let ordered = daily_recommendation_items(
                mixed_items(100),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                100,
            );
            if let Some(first) = ordered.first() {
                first_slots.insert(first.id.clone());
            }
            if let Some(second) = ordered.get(1) {
                second_slots.insert(second.id.clone());
            }
        }
        assert!(
            first_slots.len() > 1,
            "slot 0 must not stay pinned to one item across batches: {first_slots:?}"
        );
        assert!(
            second_slots.len() > 1,
            "slot 1 must not stay pinned to one item across batches: {second_slots:?}"
        );
    }

    #[test]
    fn daily_recommendations_favor_higher_ranks_while_keeping_the_tail_reachable() {
        const BATCHES: i64 = 240;
        let mut head = 0_u32;
        let mut middle = 0_u32;
        let mut tail = 0_u32;
        for batch_key in 0..BATCHES {
            let ordered = daily_recommendation_items(
                mixed_items(100),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                100,
            );
            let featured = &ordered[..RECOMMENDATION_FEATURED_LIMIT];
            if featured.iter().any(|item| item.id == "item-000") {
                head += 1;
            }
            if featured.iter().any(|item| item.id == "item-004") {
                middle += 1;
            }
            if featured.iter().any(|item| item.id == "item-060") {
                tail += 1;
            }
        }
        assert!(
            head < BATCHES as u32,
            "the top candidate must not be pinned on every single day: {head}/{BATCHES}"
        );
        assert!(
            head > middle,
            "rank 0 should surface more often than rank 4: head={head} middle={middle}"
        );
        assert!(tail > 0, "the deep tail never surfaced: {tail}/{BATCHES}");
        assert!(
            middle > tail,
            "ranking weights must favour higher ranks over the tail: middle={middle} tail={tail}"
        );
    }

    #[test]
    fn daily_recommendations_avoid_repeating_items_featured_within_seven_batches() {
        let mut history: Vec<(i64, Vec<String>)> = Vec::new();
        let mut seen_featured = HashSet::new();
        for batch_key in 100..107 {
            let ordered = daily_recommendation_items_with_cooldown(
                mixed_items(100),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                100,
                &history,
            );
            let batch_ids = ids(&ordered);
            for featured_id in batch_ids.iter().take(RECOMMENDATION_FEATURED_LIMIT) {
                assert!(
                    seen_featured.insert(featured_id.clone()),
                    "item {featured_id} repeated within 7 daily batches on batch {batch_key}"
                );
            }
            history.push((batch_key, batch_ids));
        }
        assert_eq!(seen_featured.len(), 7 * RECOMMENDATION_FEATURED_LIMIT);
    }

    #[test]
    fn daily_recommendations_fall_back_to_oldest_shown_items_when_pool_is_small() {
        let pool = mixed_items(18);
        let mut history: Vec<(i64, Vec<String>)> = Vec::new();
        let mut daily_featured: Vec<Vec<String>> = Vec::new();

        for batch_key in 200..205 {
            let ordered = daily_recommendation_items_with_cooldown(
                pool.clone(),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                pool.len(),
                &history,
            );
            let batch_ids = ids(&ordered);
            let featured = batch_ids[..RECOMMENDATION_FEATURED_LIMIT].to_vec();
            assert_eq!(featured.len(), RECOMMENDATION_FEATURED_LIMIT);
            if let Some(yesterday) = daily_featured.last() {
                let overlap = featured
                    .iter()
                    .filter(|id| yesterday.contains(id))
                    .collect::<Vec<_>>();
                assert!(
                    overlap.is_empty(),
                    "18-item pool should never repeat yesterday's items on batch {batch_key}: {overlap:?}"
                );
            }
            daily_featured.push(featured);
            history.push((batch_key, batch_ids));
        }

        // Day 1 (7) + Day 2 (7) leave 4 fresh items for Day 3, so Day 3's 3 fallback items must
        // come from Day 1 (oldest shown), never Day 2.
        let day1: HashSet<_> = daily_featured[0].iter().cloned().collect();
        let day2: HashSet<_> = daily_featured[1].iter().cloned().collect();
        let day3_reused = daily_featured[2]
            .iter()
            .filter(|id| day1.contains(*id) || day2.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(day3_reused.len(), 3);
        assert!(day3_reused.iter().all(|id| day1.contains(id)));
    }

    #[test]
    fn daily_recommendations_cap_recent_playback_items_at_five_and_relax_when_needed() {
        let mut items = (0..12)
            .map(|index| {
                let mut item = recent_playback_movie(index);
                if index % 2 == 1 {
                    item.kind = "SERIES";
                }
                item
            })
            .collect::<Vec<_>>();
        items.extend((12..30).map(|index| {
            if index % 2 == 0 {
                movie(index)
            } else {
                series_item(index)
            }
        }));

        for batch_key in 0..20 {
            let ordered = daily_recommendation_items(
                items.clone(),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                items.len(),
            );
            let featured = &ordered[..RECOMMENDATION_FEATURED_LIMIT];
            let recent_playback_count = featured.iter().filter(|item| item.recent_playback).count();
            assert!(
                recent_playback_count <= 5,
                "featured slots exceeded 5 recent-playback items: {recent_playback_count}"
            );
        }

        let all_recent = (0..10).map(recent_playback_movie).collect::<Vec<_>>();
        let relaxed =
            daily_recommendation_items(all_recent, "user-1", 20, RECOMMENDATION_FEATURED_LIMIT, 10);
        assert_eq!(relaxed[..RECOMMENDATION_FEATURED_LIMIT].len(), 7);
    }

    #[test]
    fn daily_recommendations_limit_each_item_type_in_the_featured_slots() {
        let mut items = (0..30).map(movie).collect::<Vec<_>>();
        items.extend((30..40).map(series_item));

        for batch_key in 0..20 {
            let ordered = daily_recommendation_items(
                items.clone(),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                40,
            );
            let featured = &ordered[..RECOMMENDATION_FEATURED_LIMIT];
            assert_eq!(featured.len(), RECOMMENDATION_FEATURED_LIMIT);
            assert!(
                featured.iter().filter(|item| item.kind == "MOVIE").count() <= 4,
                "too many MOVIE items in the featured prefix"
            );
            assert!(
                featured.iter().filter(|item| item.kind == "SERIES").count() <= 4,
                "too many SERIES items in the featured prefix"
            );
        }
    }

    #[test]
    fn daily_recommendations_relax_the_type_cap_when_candidates_run_out() {
        let single_kind = (0..10).map(movie).collect::<Vec<_>>();
        let ordered = daily_recommendation_items(
            single_kind,
            "user-1",
            20,
            RECOMMENDATION_FEATURED_LIMIT,
            10,
        );
        assert_eq!(ordered.len(), 10);
        assert_eq!(ordered[..RECOMMENDATION_FEATURED_LIMIT].len(), 7);
    }

    #[test]
    fn daily_recommendations_keep_the_rest_of_the_batch_in_ranking_order() {
        let ordered = daily_recommendation_items(
            mixed_items(20),
            "user-1",
            20,
            RECOMMENDATION_FEATURED_LIMIT,
            20,
        );

        assert_eq!(ordered.len(), 20);
        let mut unique = ids(&ordered);
        unique.sort();
        let mut expected = ids(&mixed_items(20));
        expected.sort();
        assert_eq!(unique, expected, "the batch must be a permutation");

        let featured = ids(&ordered[..RECOMMENDATION_FEATURED_LIMIT]);
        let expected_tail = ids(&mixed_items(20))
            .into_iter()
            .filter(|id| !featured.contains(id))
            .collect::<Vec<_>>();
        assert_eq!(
            ids(&ordered[RECOMMENDATION_FEATURED_LIMIT..]),
            expected_tail
        );
    }

    #[test]
    fn daily_recommendations_handle_empty_short_and_single_candidate_pools() {
        assert!(daily_recommendation_items(Vec::<TestItem>::new(), "user-1", 20, 7, 40).is_empty());

        let single = daily_recommendation_items(vec![movie(0)], "user-1", 20, 7, 40);
        assert_eq!(ids(&single), ["item-000"]);

        let shorter_than_featured =
            daily_recommendation_items((0..3).map(movie).collect(), "user-1", 20, 7, 40);
        assert_eq!(shorter_than_featured.len(), 3);

        let no_featured_slots = daily_recommendation_items(mixed_items(4), "user-1", 20, 0, 40);
        assert_eq!(
            ids(&no_featured_slots),
            ["item-000", "item-001", "item-002", "item-003"]
        );

        let capped_batch = daily_recommendation_items(mixed_items(20), "user-1", 20, 7, 12);
        assert_eq!(capped_batch.len(), 12);
    }

    #[test]
    fn deterministic_unit_interval_is_seeded_and_stays_inside_the_open_unit_interval() {
        for index in 0..200 {
            let value = deterministic_unit_interval("user-1", 20, &format!("item-{index:03}"));
            assert!(value > 0.0 && value < 1.0, "seed produced {value}");
        }
        // Pinned so a seed change (or an accidental process-random seed) fails loudly.
        assert_eq!(
            deterministic_unit_interval("user-1", 20, "item-001"),
            0.735_148_821_116_930_8
        );
        assert_ne!(
            deterministic_unit_interval("user-1", 20, "item-001"),
            deterministic_unit_interval("user-1", 21, "item-001")
        );
        assert_ne!(
            deterministic_unit_interval("user-1", 20, "item-001"),
            deterministic_unit_interval("user-2", 20, "item-001")
        );
        assert_ne!(
            deterministic_unit_interval("user-1", 20, "item-001"),
            deterministic_unit_interval("user-1", 20, "item-002")
        );
    }

    #[test]
    fn weighted_sampling_keys_reject_degenerate_floats() {
        assert_eq!(weighted_sampling_key(0.0, 0.5), f64::NEG_INFINITY);
        assert_eq!(weighted_sampling_key(f64::NAN, 0.5), f64::NEG_INFINITY);
        assert_eq!(weighted_sampling_key(f64::INFINITY, 0.5), f64::NEG_INFINITY);
        assert_eq!(weighted_sampling_key(0.5, 0.0), f64::NEG_INFINITY);
        assert_eq!(weighted_sampling_key(0.5, 1.0), f64::NEG_INFINITY);
        assert_eq!(weighted_sampling_key(0.5, f64::NAN), f64::NEG_INFINITY);
        assert!(weighted_sampling_key(0.5, 0.999).is_finite());
        assert!(weighted_sampling_key(0.5, 0.999) < 0.0);
        assert!(rank_weight(usize::MAX).is_finite());
        assert!(rank_weight(usize::MAX) > 0.0);
        assert!(rank_weight(0) <= 1.0);
    }
}

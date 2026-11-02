use std::{
    collections::{HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

/// Candidate rows fetched from the scoring query for one daily batch.
///
/// The batch only needs [`RECOMMENDATION_FEATURED_LIMIT`] curated slots, so the pool only has to
/// be deep enough for the weighted draw and the diversity rules to have material to work with.
pub(crate) const RECOMMENDATION_CANDIDATE_POOL: i64 = 60;

/// Leading home-carousel slots selected by [`daily_recommendation_items`].
///
/// Home requests today ask for seven recommendations; the stored batch always starts with this
/// curated prefix so any consumer with a limit up to seven receives the diversified picks.
pub(crate) const RECOMMENDATION_FEATURED_LIMIT: usize = 7;

/// The two best-scored candidates are anchored at the front of every batch, so the most relevant
/// items never lose the lottery against the rest of the pool. Anchors are exempt from the
/// diversity rules (they are always included); the rules apply to the slots drawn after them.
const ANCHOR_COUNT: usize = 2;

/// Exponent of the rank-derived sampling weight.
///
/// The SQL query ranks by a weighted score that Rust cannot see, so the selection works from the
/// rank position instead: `weight = 1 / (rank + 1)^0.8`, where `rank` is the 0-based position in
/// the score-ordered pool. `0.8` is a sublinear Zipf-like profile: it decays fast enough that the
/// head dominates (0-based rank 2 keeps ~42% of the top weight, rank 6 ~24%) but slowly enough
/// that the tail stays reachable (0-based rank 30 keeps ~6.4%), which is what makes the carousel
/// rotate instead of freezing on the top few rows.
const RANK_WEIGHT_EXPONENT: f64 = 0.8;

/// Lower bound for a rank weight so the logarithmic sampling key stays finite.
const MIN_RANK_WEIGHT: f64 = 1e-6;

/// A single `item_type` may occupy at most this many featured slots. The pool only holds movies
/// and series, so the cap leaves room for both kinds in a seven-slot carousel; it is relaxed when
/// the pool cannot satisfy it.
const MAX_FEATURED_PER_ITEM_TYPE: usize = 4;

/// The catalog fields the daily selection needs. Catalog rows are ranked by the SQL query; this
/// trait exposes only the stable identity and the diversity dimensions of a candidate.
pub(crate) trait RecommendationCandidate {
    /// Stable identity used to seed the deterministic draw. It must not change across restarts.
    fn candidate_key(&self) -> &str;

    /// `item_type` (for example `MOVIE` or `SERIES`) used by the type diversity cap.
    fn candidate_kind(&self) -> &str;

    /// Series grouping key (the `series_id` of an episode). `None` means the candidate is its own
    /// series, which is the case for movies and for a series entry itself.
    fn candidate_series_key(&self) -> Option<&str>;
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
pub(crate) fn daily_recommendation_items<T: RecommendationCandidate>(
    items: Vec<T>,
    user_id: &str,
    batch_key: i64,
    featured_limit: usize,
    batch_limit: usize,
) -> Vec<T> {
    let batch_limit = batch_limit.min(items.len());
    if batch_limit == 0 {
        return Vec::new();
    }
    let featured_limit = featured_limit.min(batch_limit);
    let curated = curated_prefix_indices(&items, user_id, batch_key, featured_limit);

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

/// Picks the curated prefix: the anchored leaders first, then a weighted draw without replacement.
///
/// Diversity is enforced while walking the draw: one item per series and at most
/// [`MAX_FEATURED_PER_ITEM_TYPE`] items per type. Constraints relax in two passes (type cap first,
/// then the series cap) so a thin pool still fills every slot instead of returning a short prefix.
fn curated_prefix_indices<T: RecommendationCandidate>(
    items: &[T],
    user_id: &str,
    batch_key: i64,
    featured_limit: usize,
) -> Vec<usize> {
    let featured_limit = featured_limit.min(items.len());
    let anchor_count = ANCHOR_COUNT.min(featured_limit);
    let mut selected = (0..anchor_count).collect::<Vec<_>>();
    if selected.len() >= featured_limit {
        return selected;
    }

    let mut type_counts: HashMap<&str, usize> = HashMap::new();
    let mut series_used: HashSet<&str> = HashSet::new();
    for index in &selected {
        if let Some(item) = items.get(*index) {
            *type_counts.entry(item.candidate_kind()).or_default() += 1;
            series_used.insert(series_group_key(item));
        }
    }

    let mut drawn = (anchor_count..items.len())
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

    let mut chosen = vec![false; items.len()];
    for index in &selected {
        if let Some(slot) = chosen.get_mut(*index) {
            *slot = true;
        }
    }

    // Strict pass first, then relax the type cap, then the series cap. Each pass only revisits
    // candidates the previous ones rejected, so a thin pool still fills every slot.
    for relaxation in [Relaxation::None, Relaxation::TypeCap, Relaxation::AllRules] {
        if selected.len() >= featured_limit {
            break;
        }
        for (_, index) in &drawn {
            if selected.len() >= featured_limit {
                break;
            }
            if chosen.get(*index).copied().unwrap_or_default() {
                continue;
            }
            let Some(item) = items.get(*index) else {
                continue;
            };
            let series_taken = series_used.contains(series_group_key(item));
            let type_count = type_counts
                .get(item.candidate_kind())
                .copied()
                .unwrap_or_default();
            let series_allowed = relaxation == Relaxation::AllRules || !series_taken;
            let type_allowed =
                relaxation != Relaxation::None || type_count < MAX_FEATURED_PER_ITEM_TYPE;
            if !series_allowed || !type_allowed {
                continue;
            }
            if let Some(slot) = chosen.get_mut(*index) {
                *slot = true;
            }
            *type_counts.entry(item.candidate_kind()).or_default() += 1;
            series_used.insert(series_group_key(item));
            selected.push(*index);
        }
    }

    selected
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relaxation {
    /// Both diversity rules apply.
    None,
    /// The per-type cap is ignored; a series still appears at most once.
    TypeCap,
    /// Every rule is ignored so the prefix is filled rather than returned short.
    AllRules,
}

fn series_group_key<T: RecommendationCandidate>(item: &T) -> &str {
    item.candidate_series_key()
        .filter(|series_id| !series_id.is_empty())
        .unwrap_or_else(|| item.candidate_key())
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
        RECOMMENDATION_FEATURED_LIMIT, RecommendationCandidate, daily_recommendation_items,
        deterministic_unit_interval, rank_weight, recommendation_library_scope_key,
        weighted_sampling_key,
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
        series: Option<String>,
    }

    impl RecommendationCandidate for TestItem {
        fn candidate_key(&self) -> &str {
            &self.id
        }

        fn candidate_kind(&self) -> &str {
            self.kind
        }

        fn candidate_series_key(&self) -> Option<&str> {
            self.series.as_deref()
        }
    }

    fn movie(index: usize) -> TestItem {
        TestItem {
            id: format!("item-{index:02}"),
            kind: "MOVIE",
            series: None,
        }
    }

    fn series_item(index: usize) -> TestItem {
        TestItem {
            id: format!("item-{index:02}"),
            kind: "SERIES",
            series: Some(format!("series-{index:02}")),
        }
    }

    /// Alternating kinds with one series per item keep the diversity rules satisfiable, so the
    /// statistics below measure the ranking weights and not the relaxation passes.
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
    fn daily_recommendations_always_lead_with_the_top_two_ranked_items() {
        for batch_key in 0..50 {
            let ordered = daily_recommendation_items(
                mixed_items(40),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                40,
            );
            assert_eq!(
                ordered.first().map(|item| item.id.as_str()),
                Some("item-00")
            );
            assert_eq!(ordered.get(1).map(|item| item.id.as_str()), Some("item-01"));
        }
    }

    #[test]
    fn daily_recommendations_show_the_top_rank_every_day_and_the_tail_rarely() {
        const BATCHES: i64 = 240;
        let mut head = 0_u32;
        let mut middle = 0_u32;
        let mut tail = 0_u32;
        for batch_key in 0..BATCHES {
            let ordered = daily_recommendation_items(
                mixed_items(40),
                "user-1",
                batch_key,
                RECOMMENDATION_FEATURED_LIMIT,
                40,
            );
            let featured = &ordered[..RECOMMENDATION_FEATURED_LIMIT];
            if featured.iter().any(|item| item.id == "item-00") {
                head += 1;
            }
            if featured.iter().any(|item| item.id == "item-02") {
                middle += 1;
            }
            if featured.iter().any(|item| item.id == "item-30") {
                tail += 1;
            }
        }
        assert_eq!(head, BATCHES as u32, "the top candidate is an anchor");
        assert!(tail > 0, "the deep tail never surfaced: {tail}/{BATCHES}");
        assert!(
            tail < BATCHES as u32 * 3 / 8,
            "the deep tail surfaced too often: {tail}/{BATCHES}"
        );
        assert!(
            middle > tail,
            "ranking weights must favour the middle of the pool: middle={middle} tail={tail}"
        );
    }

    #[test]
    fn daily_recommendations_do_not_repeat_a_series_in_the_featured_slots() {
        let mut items = mixed_items(40);
        for item in items.iter_mut().take(21).skip(2) {
            item.series = Some("series-dup".to_owned());
        }

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
                featured
                    .iter()
                    .filter(|item| item.series.as_deref() == Some("series-dup"))
                    .count()
                    <= 1,
                "the same series appeared twice in one featured prefix"
            );
            let unique = ids(&ordered).into_iter().collect::<HashSet<_>>();
            assert_eq!(unique.len(), 40, "a batch must not repeat a candidate");
        }
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
    fn daily_recommendations_relax_diversity_rules_when_candidates_run_out() {
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

        let shared_series = (0..10)
            .map(|index| TestItem {
                id: format!("item-{index:02}"),
                kind: "MOVIE",
                series: Some("series-1".to_owned()),
            })
            .collect::<Vec<_>>();
        let ordered = daily_recommendation_items(
            shared_series,
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
        assert_eq!(ids(&single), ["item-00"]);

        let shorter_than_featured =
            daily_recommendation_items((0..3).map(movie).collect(), "user-1", 20, 7, 40);
        assert_eq!(
            ids(&shorter_than_featured),
            ["item-00", "item-01", "item-02"]
        );

        let no_featured_slots = daily_recommendation_items(mixed_items(4), "user-1", 20, 0, 40);
        assert_eq!(
            ids(&no_featured_slots),
            ["item-00", "item-01", "item-02", "item-03"]
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

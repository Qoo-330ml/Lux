use super::{DatabaseBackend, adapt_sql_for_backend};
use crate::storage::sql_templates::{
    SqlTemplateCache, filesystem_entries_by_paths_template,
    manifest_directory_frontier_complete_template, manifest_directory_frontier_insert_template,
};
use sqlx::{Connection, SqliteConnection, sqlite::SqlitePoolOptions};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::Arc,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};

struct CountingSystemAllocator;

static CAPTURE_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOCATION_BYTES: AtomicUsize = AtomicUsize::new(0);
static REALLOCATION_COUNT: AtomicUsize = AtomicUsize::new(0);
static REALLOCATION_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static BENCHMARK_ALLOCATOR: CountingSystemAllocator = CountingSystemAllocator;

unsafe impl GlobalAlloc for CountingSystemAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc(layout) };
        if !result.is_null() && CAPTURE_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        result
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let result = unsafe { System.alloc_zeroed(layout) };
        if !result.is_null() && CAPTURE_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        result
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() && CAPTURE_ALLOCATIONS.load(Ordering::Relaxed) {
            REALLOCATION_COUNT.fetch_add(1, Ordering::Relaxed);
            REALLOCATION_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        result
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
}

#[derive(Clone, Copy)]
struct AllocationSnapshot {
    allocation_count: usize,
    allocation_bytes: usize,
    reallocation_count: usize,
    reallocation_bytes: usize,
}

fn measure_allocations<T>(measure: impl FnOnce() -> T) -> (T, AllocationSnapshot) {
    ALLOCATION_COUNT.store(0, Ordering::Relaxed);
    ALLOCATION_BYTES.store(0, Ordering::Relaxed);
    REALLOCATION_COUNT.store(0, Ordering::Relaxed);
    REALLOCATION_BYTES.store(0, Ordering::Relaxed);
    CAPTURE_ALLOCATIONS.store(true, Ordering::SeqCst);
    let result = black_box(measure());
    CAPTURE_ALLOCATIONS.store(false, Ordering::SeqCst);

    (
        result,
        AllocationSnapshot {
            allocation_count: ALLOCATION_COUNT.load(Ordering::Relaxed),
            allocation_bytes: ALLOCATION_BYTES.load(Ordering::Relaxed),
            reallocation_count: REALLOCATION_COUNT.load(Ordering::Relaxed),
            reallocation_bytes: REALLOCATION_BYTES.load(Ordering::Relaxed),
        },
    )
}

fn placeholder_query(row_count: usize) -> String {
    let values = std::iter::repeat_n("(?)", row_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!("WITH incoming(path) AS (VALUES {values}) SELECT path FROM incoming")
}

fn sql_with_question_marks(count: usize) -> String {
    let placeholders = std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ");
    format!("SELECT COUNT(*) FROM input_paths WHERE path IN ({placeholders})")
}

fn report_allocation_case(
    name: &str,
    iterations: usize,
    elapsed: std::time::Duration,
    snapshot: AllocationSnapshot,
) {
    println!(
        "allocation_case={name} iterations={iterations} elapsed_ns_per_op={} allocs_per_op={:.2} alloc_bytes_per_op={:.2} reallocs_per_op={:.2} realloc_bytes_per_op={:.2}",
        elapsed.as_nanos() / u128::try_from(iterations).unwrap_or(1),
        snapshot.allocation_count as f64 / iterations as f64,
        snapshot.allocation_bytes as f64 / iterations as f64,
        snapshot.reallocation_count as f64 / iterations as f64,
        snapshot.reallocation_bytes as f64 / iterations as f64,
    );
}

fn measure_operation<T>(
    iterations: usize,
    mut allocation_operation: impl FnMut() -> T,
    mut timed_operation: impl FnMut() -> T,
) -> (std::time::Duration, AllocationSnapshot) {
    let (_, snapshot) = measure_allocations(|| {
        for _ in 0..iterations {
            drop(black_box(allocation_operation()));
        }
    });
    let started = Instant::now();
    for _ in 0..iterations {
        drop(black_box(timed_operation()));
    }
    (started.elapsed(), snapshot)
}

fn sqlx_default_statement_cache_capacity() -> usize {
    100
}

async fn execute_count_query(
    connection: &mut SqliteConnection,
    query: Arc<str>,
    parameter_count: usize,
) -> Result<i64, sqlx::Error> {
    let mut statement = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query));
    for _ in 0..parameter_count {
        statement = statement.bind(1_i64);
    }
    statement.fetch_one(connection).await
}

fn filesystem_path_template(cache: &SqlTemplateCache, capacity: u16) -> Arc<str> {
    filesystem_entries_by_paths_template(cache, DatabaseBackend::Postgres, capacity)
        .expect("valid filesystem path template")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "run explicitly to record the LUX-562 SQL construction baseline"]
async fn dynamic_sql_allocation_baseline() -> Result<(), Box<dyn std::error::Error>> {
    let iterations = 2_000;
    let static_sql =
        "SELECT id FROM filesystem_entries WHERE library_root_id = ? AND relative_path = ?";
    let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, static_sql);
    assert_eq!(adapted.as_str(), static_sql);
    println!(
        "sqlite_static_adapter_pointer_preserved={}",
        std::ptr::eq(static_sql.as_ptr(), adapted.as_str().as_ptr())
    );
    let (elapsed, snapshot) = measure_operation(
        iterations,
        || adapt_sql_for_backend(DatabaseBackend::Sqlite, static_sql),
        || adapt_sql_for_backend(DatabaseBackend::Sqlite, static_sql),
    );
    report_allocation_case("sqlite_static_sql_adapter", iterations, elapsed, snapshot);

    let owned_inputs_for_allocations = (0..iterations)
        .map(|_| static_sql.to_owned())
        .collect::<Vec<_>>();
    let owned_inputs_for_timing = (0..iterations)
        .map(|_| static_sql.to_owned())
        .collect::<Vec<_>>();
    let mut owned_inputs_for_allocations = owned_inputs_for_allocations.into_iter();
    let mut owned_inputs_for_timing = owned_inputs_for_timing.into_iter();
    let (elapsed, snapshot) = measure_operation(
        iterations,
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Sqlite,
                sqlx::AssertSqlSafe(
                    owned_inputs_for_allocations
                        .next()
                        .expect("prebuilt SQL input"),
                ),
            )
        },
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Sqlite,
                sqlx::AssertSqlSafe(owned_inputs_for_timing.next().expect("prebuilt SQL input")),
            )
        },
    );
    report_allocation_case("sqlite_owned_sql_adapter", iterations, elapsed, snapshot);

    let postgres_sql = sql_with_question_marks(120);
    let postgres_inputs_for_allocations = (0..iterations)
        .map(|_| postgres_sql.clone())
        .collect::<Vec<_>>();
    let postgres_inputs_for_timing = (0..iterations)
        .map(|_| postgres_sql.clone())
        .collect::<Vec<_>>();
    let mut postgres_inputs_for_allocations = postgres_inputs_for_allocations.into_iter();
    let mut postgres_inputs_for_timing = postgres_inputs_for_timing.into_iter();
    let (elapsed, snapshot) = measure_operation(
        iterations,
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Postgres,
                sqlx::AssertSqlSafe(
                    postgres_inputs_for_allocations
                        .next()
                        .expect("prebuilt PostgreSQL SQL"),
                ),
            )
        },
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Postgres,
                sqlx::AssertSqlSafe(
                    postgres_inputs_for_timing
                        .next()
                        .expect("prebuilt PostgreSQL SQL"),
                ),
            )
        },
    );
    report_allocation_case(
        "postgres_120_parameter_adapter",
        iterations,
        elapsed,
        snapshot,
    );

    let template_cache = SqlTemplateCache::default();
    let template_capacity = 128_u16;
    let cached_template = filesystem_path_template(&template_cache, template_capacity);
    let source_query = format!(
        "SELECT id FROM filesystem_entries WHERE library_root_id = ? AND relative_path IN ({})",
        std::iter::repeat_n("?", usize::from(template_capacity))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let expected_template = adapt_sql_for_backend(
        DatabaseBackend::Postgres,
        sqlx::AssertSqlSafe(source_query.clone()),
    );
    assert_eq!(cached_template.as_ref(), expected_template.as_str());

    let adapter_inputs_for_allocations = (0..iterations)
        .map(|_| source_query.clone())
        .collect::<Vec<_>>();
    let adapter_inputs_for_timing = (0..iterations)
        .map(|_| source_query.clone())
        .collect::<Vec<_>>();
    let mut adapter_inputs_for_allocations = adapter_inputs_for_allocations.into_iter();
    let mut adapter_inputs_for_timing = adapter_inputs_for_timing.into_iter();
    let (elapsed, snapshot) = measure_operation(
        iterations,
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Postgres,
                sqlx::AssertSqlSafe(
                    adapter_inputs_for_allocations
                        .next()
                        .expect("prebuilt 128-parameter SQL"),
                ),
            )
        },
        || {
            adapt_sql_for_backend(
                DatabaseBackend::Postgres,
                sqlx::AssertSqlSafe(
                    adapter_inputs_for_timing
                        .next()
                        .expect("prebuilt 128-parameter SQL"),
                ),
            )
        },
    );
    report_allocation_case(
        "postgres_128_parameter_adapter",
        iterations,
        elapsed,
        snapshot,
    );

    let (elapsed, snapshot) = measure_operation(
        iterations,
        || filesystem_path_template(&template_cache, template_capacity),
        || filesystem_path_template(&template_cache, template_capacity),
    );
    report_allocation_case(
        "postgres_128_parameter_template_cache_hit",
        iterations,
        elapsed,
        snapshot,
    );

    let miss_iterations = 100;
    let (elapsed, snapshot) = measure_operation(
        miss_iterations,
        || filesystem_path_template(&SqlTemplateCache::default(), template_capacity),
        || filesystem_path_template(&SqlTemplateCache::default(), template_capacity),
    );
    report_allocation_case(
        "postgres_128_parameter_template_cache_miss",
        miss_iterations,
        elapsed,
        snapshot,
    );

    for row_count in [1, 100, 200, 2_900, 5_000] {
        let case_iterations = if row_count >= 2_900 { 40 } else { 2_000 };
        let (elapsed, snapshot) = measure_operation(
            case_iterations,
            || placeholder_query(row_count),
            || placeholder_query(row_count),
        );
        report_allocation_case(
            &format!("values_cte_{row_count}_rows"),
            case_iterations,
            elapsed,
            snapshot,
        );
    }

    for path_length in [32_usize, 128, 512] {
        let paths = (0..200)
            .map(|index| format!("{}-{index:06}", "p".repeat(path_length.saturating_sub(7))))
            .collect::<Vec<_>>();
        let query: Arc<str> = sql_with_question_marks(paths.len()).into();
        let (elapsed, snapshot) = measure_operation(
            100,
            || {
                let mut statement = sqlx::query::<sqlx::Any>(sqlx::AssertSqlSafe(query.clone()));
                for path in &paths {
                    statement = statement.bind(path.as_str());
                }
                black_box(statement)
            },
            || {
                let mut statement = sqlx::query::<sqlx::Any>(sqlx::AssertSqlSafe(query.clone()));
                for path in &paths {
                    statement = statement.bind(path.as_str());
                }
                black_box(statement)
            },
        );
        report_allocation_case(
            &format!("any_query_200_paths_{path_length}_bytes"),
            100,
            elapsed,
            snapshot,
        );
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    let mut connection = pool.acquire().await?;
    let query_shapes: Vec<Arc<str>> = (1..=120)
        .map(|parameter_count| {
            let selects = std::iter::repeat_n("SELECT ?", parameter_count)
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            format!("SELECT COUNT(*) FROM ({selects})").into()
        })
        .collect::<Vec<_>>();

    let cold_started = Instant::now();
    for (index, query) in query_shapes.iter().enumerate() {
        let count =
            execute_count_query(&mut connection, query.clone(), index.saturating_add(1)).await?;
        assert_eq!(count, i64::try_from(index + 1)?);
    }
    let cold_elapsed = cold_started.elapsed();
    let resident_after_cold = connection.cached_statements_size();

    let warm_started = Instant::now();
    for index in (20..120).rev() {
        let count =
            execute_count_query(&mut connection, query_shapes[index].clone(), index + 1).await?;
        assert_eq!(count, i64::try_from(index + 1)?);
    }
    let warm_elapsed = warm_started.elapsed();

    let evicted_started = Instant::now();
    for (index, query) in query_shapes.iter().take(20).enumerate() {
        let count = execute_count_query(&mut connection, query.clone(), index + 1).await?;
        assert_eq!(count, i64::try_from(index + 1)?);
    }
    let evicted_elapsed = evicted_started.elapsed();
    let resident_after_reuse = connection.cached_statements_size();

    println!(
        "sqlite_statement_cache capacity={} unique_shapes={} resident_after_cold={} resident_after_reuse={} cold_us={} cached_replay_100_us={} evicted_replay_20_us={}",
        sqlx_default_statement_cache_capacity(),
        query_shapes.len(),
        resident_after_cold,
        resident_after_reuse,
        cold_elapsed.as_micros(),
        warm_elapsed.as_micros(),
        evicted_elapsed.as_micros(),
    );
    assert_eq!(resident_after_cold, sqlx_default_statement_cache_capacity());
    assert_eq!(
        resident_after_reuse,
        sqlx_default_statement_cache_capacity()
    );

    drop(connection);
    pool.close().await;
    Ok(())
}

fn legacy_manifest_frontier_insert_sql(row_count: usize) -> String {
    let values = std::iter::repeat_n("(?, ?, ?, 'PENDING')", row_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "INSERT INTO scan_manifest_directories (
             manifest_id, library_root_id, relative_path, state
         ) VALUES {values}
         ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
    )
}

fn legacy_manifest_frontier_complete_sql(row_count: usize) -> String {
    let placeholders = std::iter::repeat_n("?", row_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "UPDATE scan_manifest_directories
         SET state = 'COMPLETE', error = NULL, updated_at = unixepoch()
         WHERE manifest_id = ? AND library_root_id = ? AND state <> 'COMPLETE'
           AND relative_path IN ({placeholders})"
    )
}

#[test]
#[ignore = "run explicitly to compare Manifest directory frontier SQL construction"]
fn manifest_directory_frontier_sql_construction_benchmark() {
    let cache = SqlTemplateCache::default();
    for (batch_name, row_count, iterations) in
        [("full_batch", 200, 2_000), ("tail_batch", 17, 5_000)]
    {
        manifest_directory_frontier_insert_template(&cache, DatabaseBackend::Sqlite, row_count)
            .expect("warm directory insert template");
        manifest_directory_frontier_complete_template(&cache, DatabaseBackend::Sqlite, row_count)
            .expect("warm directory completion template");

        let (elapsed, snapshot) = measure_operation(
            iterations,
            || legacy_manifest_frontier_insert_sql(row_count),
            || legacy_manifest_frontier_insert_sql(row_count),
        );
        report_allocation_case(
            &format!("manifest_directory_insert_{batch_name}_dynamic"),
            iterations,
            elapsed,
            snapshot,
        );
        let (elapsed, snapshot) = measure_operation(
            iterations,
            || {
                manifest_directory_frontier_insert_template(
                    &cache,
                    DatabaseBackend::Sqlite,
                    row_count,
                )
                .expect("cached directory insert template")
            },
            || {
                manifest_directory_frontier_insert_template(
                    &cache,
                    DatabaseBackend::Sqlite,
                    row_count,
                )
                .expect("cached directory insert template")
            },
        );
        report_allocation_case(
            &format!("manifest_directory_insert_{batch_name}_template_hit"),
            iterations,
            elapsed,
            snapshot,
        );

        let (elapsed, snapshot) = measure_operation(
            iterations,
            || legacy_manifest_frontier_complete_sql(row_count),
            || legacy_manifest_frontier_complete_sql(row_count),
        );
        report_allocation_case(
            &format!("manifest_directory_complete_{batch_name}_dynamic"),
            iterations,
            elapsed,
            snapshot,
        );
        let (elapsed, snapshot) = measure_operation(
            iterations,
            || {
                manifest_directory_frontier_complete_template(
                    &cache,
                    DatabaseBackend::Sqlite,
                    row_count,
                )
                .expect("cached directory completion template")
            },
            || {
                manifest_directory_frontier_complete_template(
                    &cache,
                    DatabaseBackend::Sqlite,
                    row_count,
                )
                .expect("cached directory completion template")
            },
        );
        report_allocation_case(
            &format!("manifest_directory_complete_{batch_name}_template_hit"),
            iterations,
            elapsed,
            snapshot,
        );
    }
}

//! Removal of the personal image switch must preserve every other preference.
use luxd::{config::Config, storage::Database};
use sqlx::{Row, migrate::Migrator, sqlite::SqlitePoolOptions};

#[tokio::test]
async fn fresh_database_has_no_personal_image_preference() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp.path().join("config"),
    })
    .await?;
    let columns = sqlx::query("PRAGMA table_info(user_account_preferences)")
        .fetch_all(database.pool())
        .await?;
    assert!(
        columns
            .iter()
            .all(|column| column.get::<String, _>("name") != "optimized_images")
    );
    Ok(())
}

#[tokio::test]
async fn upgrading_old_image_preferences_preserves_other_values()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    let migrator = sqlx::migrate!("./migrations");
    let old = Migrator::with_migrations(
        migrator
            .iter()
            .filter(|m| m.version <= 206)
            .cloned()
            .collect(),
    );
    old.run(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
        VALUES ('image-user', 'image-user', 'Images', 'test');
        INSERT INTO user_account_preferences (user_id, theme, accent_color, auto_play_next_episode,
            optimized_images, version, updated_at)
        VALUES ('image-user', 'light', 'ocean', 0, 1, 7, 1234);",
    )
    .execute(&pool)
    .await?;
    migrator.run(&pool).await?;
    let columns = sqlx::query("PRAGMA table_info(user_account_preferences)")
        .fetch_all(&pool)
        .await?;
    assert!(
        columns
            .iter()
            .all(|column| column.get::<String, _>("name") != "optimized_images")
    );
    let values: (String, String, i64, i64, i64) = sqlx::query_as(
        "SELECT theme, accent_color, auto_play_next_episode, version, updated_at
         FROM user_account_preferences WHERE user_id = 'image-user'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(values, ("light".into(), "ocean".into(), 0, 7, 1234));
    Ok(())
}

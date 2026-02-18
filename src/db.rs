#![forbid(unsafe_code)]

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;
use tracing::info;

pub async fn connect() -> anyhow::Result<Option<PgPool>> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            info!("DATABASE_URL not set — running without database (anonymous-only mode)");
            return Ok(None);
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(Duration::from_secs(3))
        .connect(&url)
        .await?;

    info!("Connected to PostgreSQL");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await?;

    info!("Database migrations applied");

    Ok(Some(pool))
}

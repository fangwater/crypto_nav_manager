use sqlx::{Executor, Postgres, pool::PoolOptions, postgres::PgPoolOptions};

pub fn pool_options(max_connections: u32, read_only: bool) -> PoolOptions<Postgres> {
    let options = PgPoolOptions::new().max_connections(max_connections);
    if read_only {
        options.after_connect(|connection, _metadata| {
            Box::pin(async move {
                connection
                    .execute("SET default_transaction_read_only = on")
                    .await?;
                Ok(())
            })
        })
    } else {
        options
    }
}

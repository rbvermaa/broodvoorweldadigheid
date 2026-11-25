#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    BakeDays(#[from] bakedays::Error),
    #[error(transparent)]
    Orders(#[from] orders::Error),
    #[error("job queue error: {0}")]
    Queue(#[from] apalis_postgres::Error),
    #[error("job worker stopped: {0}")]
    Worker(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Outbox(#[from] notify::OutboxError),
    #[error(transparent)]
    Customers(#[from] auth::customer::Error),
    #[error(transparent)]
    Polls(#[from] notify::polls::PollError),
}

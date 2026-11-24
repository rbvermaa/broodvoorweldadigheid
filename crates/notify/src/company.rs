use sqlx::PgConnection;

use crate::compose::Company;

/// The company details from the settings row. Missing KvK number or address come back
/// empty, and composing an email then fails with [`crate::ComposeError::Company`].
///
/// # Errors
/// A database error.
pub async fn load_company(conn: &mut PgConnection) -> Result<Company, sqlx::Error> {
    sqlx::query_as!(
        Company,
        r#"SELECT name, coalesce(address, '') AS "address!", coalesce(kvk, '') AS "kvk!"
        FROM company WHERE id = 1"#,
    )
    .fetch_one(conn)
    .await
}

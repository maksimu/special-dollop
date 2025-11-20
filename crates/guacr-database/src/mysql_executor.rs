// MySQL query execution using sqlx

use crate::query_executor::QueryResult;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

/// Execute MySQL query and return results
pub async fn execute_mysql_query(
    pool: &MySqlPool,
    query: &str,
) -> Result<QueryResult, sqlx::Error> {
    // Execute query
    let rows = sqlx::query(query).fetch_all(pool).await?;

    if rows.is_empty() {
        // No results (INSERT, UPDATE, DELETE, etc.)
        return Ok(QueryResult {
            columns: vec!["Status".to_string()],
            rows: vec![vec!["Query executed successfully".to_string()]],
        });
    }

    // Get column count from first row
    let column_count = rows[0].len();
    let mut columns = Vec::new();

    // Get column names - sqlx doesn't provide column names directly from rows
    // We'll use generic column names for now
    // TODO: Use query metadata to get actual column names
    for i in 0..column_count {
        columns.push(format!("Column{}", i + 1));
    }

    // Extract rows
    let mut result = QueryResult::new(columns);

    for row in rows {
        let mut row_data = Vec::new();
        for i in 0..row.len() {
            // Try to get value as string
            let value = if let Ok(val) = row.try_get::<String, _>(i) {
                val
            } else if let Ok(val) = row.try_get::<i64, _>(i) {
                val.to_string()
            } else if let Ok(val) = row.try_get::<f64, _>(i) {
                val.to_string()
            } else if let Ok(val) = row.try_get::<bool, _>(i) {
                val.to_string()
            } else {
                "<unable to convert>".to_string()
            };
            row_data.push(value);
        }
        result.add_row(row_data);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_result_structure() {
        let result = QueryResult {
            columns: vec!["id".to_string(), "name".to_string()],
            rows: vec![vec!["1".to_string(), "test".to_string()]],
        };

        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.rows.len(), 1);
    }
}

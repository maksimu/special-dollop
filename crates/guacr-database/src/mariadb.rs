// MariaDB is MySQL wire protocol compatible
// Reuse MySQL handler

use crate::mysql::MySqlHandler;

/// MariaDB handler - reuses MySQL implementation
///
/// MariaDB uses the same wire protocol as MySQL, so we can reuse the MySQL handler.
pub type MariaDbHandler = MySqlHandler;

#[cfg(test)]
mod tests {
    use super::*;
    use guacr_handlers::ProtocolHandler;

    #[test]
    fn test_mariadb_is_mysql_compatible() {
        let handler = MariaDbHandler::with_defaults();
        // MariaDB uses same handler, but will be registered as "mariadb"
        assert_eq!(handler.name(), "mysql");
    }
}

# KCM Database Plugins - Quick Reference Guide

## What Are Database Plugins?

Native C/C++ shared library plugins (.so files) that allow interactive database terminal access through the Guacamole web interface. Users connect through their web browser and get a terminal-like interface to execute SQL queries against MySQL, PostgreSQL, or SQL Server databases.

---

## Quick Facts

| Aspect | Details |
|--------|---------|
| **Plugin Type** | Native C shared libraries (.so files) |
| **Supported Databases** | MySQL 5.7+, MariaDB, PostgreSQL, Microsoft SQL Server |
| **Integration** | guacd daemon plugins (Apache Guacamole) |
| **Architecture** | Modular: shared DB library + 3 DB-specific implementations |
| **Build System** | Autotools (Autoconf/Automake) |
| **Package** | Single RPM: `kcm-libguac-client-db` (version 1.6.0) |
| **Source Location** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/` |

---

## Database Plugin Files

### Shared Library (Common Code)
```
src/db/libguac-client-db.so
├── Pseudoterminal management
├── Result set formatting
├── CSV export/import core logic
├── Clipboard handling
├── Terminal recording
└── Common settings parsing
```

### Database-Specific Plugins
```
src/mysql/libguac-client-mysql.so       (MySQL/MariaDB)
src/postgres/libguac-client-postgres.so (PostgreSQL)
src/sql-server/libguac-client-sql-server.so (Microsoft SQL Server)
```

---

## How It Works - The Flow

```
User Opens Web Browser
        ↓
Launches "mysql" connection from guacamole-client
        ↓
guacamole-client connects to guacd (port 4822)
        ↓
guacd loads libguac-client-mysql.so plugin
        ↓
Plugin establishes connection to actual MySQL database
        ↓
User sees terminal with "mysql>" prompt
        ↓
User types SQL queries
        ↓
Plugin executes queries and displays results
        ↓
Session can be recorded/typewritten for audit
```

---

## Key Components Breakdown

### 1. Connection Flow
```
Protocol Definition (JSON)
  ↓ defines fields like hostname, port, username, password
  ↓
guacamole-client Web UI
  ↓ user enters connection details
  ↓
Guacamole Protocol Message → guacd
  ↓ sends credentials over secure channel
  ↓
Plugin initialization (guac_client_init)
  ↓ parses arguments, creates DB connection
  ↓
Terminal session
  ↓ user can now execute queries
```

### 2. Threading Model
Each connection uses:
- **Input Thread** - Reads keypresses from web client, writes to terminal
- **Output Thread** - Reads terminal output, sends to web client
- **Query Handler** - Processes SQL commands
- **Synchronization** - Pthread mutexes protect shared resources

### 3. Authentication
- Credentials passed via Guacamole protocol (encrypted)
- Parsed from connection arguments
- Never stored by plugins (stateless)
- Passed to native database client library:
  - MySQL: `mysql_real_connect()`
  - PostgreSQL: `PQconnectdb()`
  - SQL Server: FreeTDS login

---

## Build Components

### Build Dependencies (What's Needed)
```
Compiler Tools:  gcc, autoconf, automake, libtool
Graphics:        cairo-devel
Testing:         CUnit-devel (unit tests)
KCM Libraries:   kcm-libguac-devel, kcm-libguac-terminal-devel
Database Libs:   mariadb-devel, postgresql-devel, kcm-libsybdb-devel
CLI:             kcm-libedit-devel (Keeper-modified readline)
Terminal:        ncurses-devel
```

### Build Process
```bash
1. autoreconf -fi              # Generate configure script
2. ./configure --disable-static # Configure without static libs
3. make                         # Compile three .so files
4. make check                   # Run unit tests
5. make install                 # Install to /usr/lib64/
```

---

## Implementation Details by Database

### MySQL/MariaDB
- **Connection Library**: libmysqlclient
- **Export Method**: `INTO LOCAL OUTFILE`
- **Import Method**: `LOAD DATA INFILE`
- **Special Feature**: Unix socket support for localhost
- **Location**: `src/mysql/`

### PostgreSQL
- **Connection Library**: libpq
- **Export Method**: `COPY TO` protocol
- **Import Method**: `\COPY` meta-command
- **Special Features**: 
  - PostgreSQL meta-commands (\d, \l, \dt, etc.)
  - Transaction tracking
- **Location**: `src/postgres/`

### SQL Server
- **Connection Library**: FreeTDS/libsybdb (TDS protocol)
- **Export Method**: BCP (Bulk Copy Protocol)
- **Import Method**: BCP
- **Special Features**:
  - TDS version selection (7.0-8.0)
  - Encryption control
  - Certificate verification options
  - GO batch command support
- **Location**: `src/sql-server/`

---

## Handler Functions (Plugin Interface)

All plugins implement these callbacks:

```c
guac_client_init()              // Called when plugin loads
guac_DBTYPE_user_join_handler() // When user connects
guac_DBTYPE_user_leave_handler()// When user disconnects
guac_DBTYPE_client_free_handler()// When session ends
guac_DBTYPE_query_handler()     // To process SQL queries
guac_DBTYPE_argv_callback()     // For runtime settings
```

---

## Configuration Options (from .json Protocol Definitions)

### All Databases Support:
- **Network**: hostname, port
- **Authentication**: username, password
- **Database**: database name, disable CSV export/import
- **Display**: font name, font size, color scheme, scrollback
- **Clipboard**: buffer size (262KB/1MB/10MB), disable copy/paste
- **Recording**: typescript path, recording path
- **Wake-on-LAN**: MAC address, broadcast, wait time

### SQL Server Only:
- **Protocol Version**: TDS 7.0, 7.1, 7.2, 7.3, 7.4, 8.0
- **Encryption**: force encryption, disable cert verification
- **CA Certificate**: custom certificate in PEM format

---

## File Locations Quick Map

| What | Where |
|-----|-------|
| Build spec | `/core/packages/kcm-libguac-client-db/kcm-libguac-client-db.spec` |
| MySQL code | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/mysql/` |
| PostgreSQL code | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/postgres/` |
| SQL Server code | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/sql-server/` |
| Shared code | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/db/` |
| Protocol defs | `/core/packages/kcm-guacamole-client/extra/{mysql\|postgres\|sql-server}.json` |
| FreeTDS | `/core/packages/kcm-libsybdb/kcm-libsybdb.spec` |
| JDBC Drivers | `/core/packages/kcm-mariadb-java-client/`, `/core/packages/kcm-mssql-jdbc/` |

---

## Key KCM Modifications

1. **Copyright Headers**: All files marked "Keeper Security, Inc."
2. **libedit Integration**: Uses `editkcm` (Keeper-modified readline)
3. **Session Recording**: Full command and output capture with timestamps
4. **Clipboard Management**: Configurable sizes, mutex-protected
5. **Wake-on-LAN**: Optional pre-connection WoL packets
6. **Bug Fixes**: KCM-446 patch fixed SQL syntax errors

---

## Recent Changes (Version 1.6.0)

- v1.6.0-5: Corrected WoL wording (KCM-459)
- v1.6.0-4: Fixed SQL command line artifacts (KCM-443)
- v1.6.0-2: Added MSSQL connection/encryption options (KCM-440)
- v1.5.5-6: Added clipboard buffer size configuration (KCM-405)

---

## Dependencies Chart

```
guacamole-client
    ↓ WebSocket
    ↓
guacd (guacamole-server)
    ↓ dlopen/dlsym
    ↓
libguac-client-mysql.so ──→ libguac-client-db.so ──→ libguac (protocol)
libguac-client-postgres.so ─→ (shared library)     → libguac-terminal
libguac-client-sql-server.so ↓                      (terminal emulation)
    ↓ native DB protocols
    ↓
MySQL Server (TCP port 3306)
PostgreSQL Server (TCP port 5432)
SQL Server (TDS protocol TCP port 1433)
```

---

## Related Java JDBC Drivers

These connect to the database to store/retrieve Guacamole configuration:
- `kcm-mariadb-java-client` v2.7.12 - Used by guacamole-client for MySQL backend
- `kcm-mssql-jdbc` v9.4.1 - Used by guacamole-client for SQL Server backend

---

## Testing

Each plugin includes unit tests:
- `src/mysql/tests/test_parse.c` - MySQL query parsing
- `src/postgres/tests/test_parse.c`, `test_util.c` - PostgreSQL tests
- `src/sql-server/tests/test_parse.c`, `test_csv.c` - SQL Server tests
- `src/db/tests/test_parse.c`, `test_util.c` - Shared library tests

Run tests with: `make check`

---

## Security Features

1. **Credential Handling**: Passed via secure Guacamole protocol, never stored
2. **Session Recording**: Full audit trail of commands and output
3. **Multi-user Control**: Join/leave handlers for shared connections
4. **Clipboard Control**: Can disable copy/paste per-connection
5. **Encryption Support**: Optional encryption for SQL Server
6. **Certificate Validation**: Optional hostname verification control

---

## Performance Characteristics

- **Memory**: Shared library reduces per-connection overhead
- **Threading**: Per-connection threads prevent blocking
- **Pseudoterminals**: Efficient I/O buffering
- **Buffer Management**: Configurable clipboard sizes prevent exhaustion
- **Synchronization**: Minimal mutex contention for shared resources

---

## Common Use Cases

1. **Remote Database Access**: Access production databases from anywhere via web browser
2. **Audited SQL Execution**: All commands logged with timestamps
3. **Non-graphical Access**: Terminals where GUI clients can't be installed
4. **Shared Database Sessions**: Multiple team members on same connection
5. **Data Import/Export**: CSV functionality for bulk operations

---

## Troubleshooting Quick Links

- **Plugin fails to load**: Check kcm-libguac-devel installed
- **Connection hangs**: Check network connectivity, firewall rules
- **Authentication fails**: Verify credentials, check database user permissions
- **Session recording fails**: Check recording path permissions, disk space
- **CSV import/export fails**: Verify database permissions for file operations

---

For detailed information, see: `/Users/mroberts/Documents/kcm/DATABASE_PLUGINS_ANALYSIS.md`

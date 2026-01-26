# KCM Database Plugins Comprehensive Analysis

## Executive Summary

This KCM (Keeper Connection Manager) codebase includes a sophisticated database connection system implemented as native C/C++ plugins for the guacd daemon. The system supports three major database platforms (MySQL/MariaDB, PostgreSQL, and Microsoft SQL Server) through a modular architecture that integrates seamlessly with Apache Guacamole.

---

## 1. Database Plugins Overview

### 1.1 Supported Database Systems

Three database protocols are fully implemented as native plugins:

| Database | Plugin Name | Version | Location |
|----------|-------------|---------|----------|
| MySQL/MariaDB | `libguac-client-mysql` | 1.6.0 | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/mysql/` |
| PostgreSQL | `libguac-client-postgres` | 1.6.0 | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/postgres/` |
| Microsoft SQL Server | `libguac-client-sql-server` | 1.6.0 | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/sql-server/` |

All three plugins are packaged in a single RPM: **kcm-libguac-client-db** (Release: 5)

### 1.2 Plugin Implementation Type

**Type**: Native C shared libraries (.so files) loaded dynamically by guacd daemon at runtime

- **MySQL/MariaDB**: `libguac-client-mysql.so` - Uses native MariaDB C API
- **PostgreSQL**: `libguac-client-postgres.so` - Uses libpq (PostgreSQL C API)
- **SQL Server**: `libguac-client-sql-server.so` - Uses FreeTDS/libsybdb (TDS protocol)

---

## 2. Codebase Location and Structure

### 2.1 Directory Tree

```
/core/packages/kcm-libguac-client-db/
├── kcm-libguac-client-db.spec              # RPM spec file (build definition)
├── extra/libguac-client-db/
│   ├── configure.ac                         # Autotools configuration
│   ├── Makefile.am                          # Automake build file
│   ├── NOTICE                               # License/notices
│   ├── README.md                            # Documentation
│   │
│   ├── src/
│   │   ├── db/                              # SHARED LIBRARY - Common DB functionality
│   │   │   ├── argv.c/h                     # Argument parsing
│   │   │   ├── client.c/h                   # Client state management
│   │   │   ├── clipboard.c/h                # Clipboard operations
│   │   │   ├── export.c/h                   # CSV export functionality
│   │   │   ├── help.c/h                     # CLI help system
│   │   │   ├── input.c/h                    # Input handling
│   │   │   ├── parse.c/h                    # Query parsing
│   │   │   ├── pipe.c/h                     # Pipe communication
│   │   │   ├── result.c/h                   # Result set handling
│   │   │   ├── settings.c/h                 # Connection settings
│   │   │   ├── status.c/h                   # Status information
│   │   │   ├── util.c/h                     # Utility functions
│   │   │   ├── tests/                       # Unit tests
│   │   │   │   ├── test_parse.c
│   │   │   │   └── test_util.c
│   │   │   └── Makefile.am
│   │   │
│   │   ├── mysql/                           # MySQL-SPECIFIC IMPLEMENTATION
│   │   │   ├── argv.c/h                     # MySQL argument handling
│   │   │   ├── export.c/h                   # MySQL export (INTO LOCAL OUTFILE)
│   │   │   ├── help.c                       # MySQL CLI help
│   │   │   ├── import.c/h                   # CSV import functionality
│   │   │   ├── mysql.c/h                    # Main MySQL client (guac_client_init)
│   │   │   ├── parse.c/h                    # MySQL query parsing
│   │   │   ├── result.c/h                   # MySQL result handling
│   │   │   ├── settings.c/h                 # MySQL connection settings
│   │   │   ├── status.c/h                   # MySQL status display
│   │   │   ├── user.c/h                     # MySQL user join/leave handlers
│   │   │   ├── tests/test_parse.c           # MySQL unit tests
│   │   │   └── Makefile.am
│   │   │
│   │   ├── postgres/                        # PostgreSQL-SPECIFIC IMPLEMENTATION
│   │   │   ├── argv.c/h
│   │   │   ├── export.c/h                   # PostgreSQL COPY export
│   │   │   ├── help.c
│   │   │   ├── import.c/h                   # CSV import with \COPY
│   │   │   ├── meta.c/h                     # Meta-commands (\d, \l, etc.)
│   │   │   ├── parse.c/h
│   │   │   ├── postgres.c/h                 # Main PostgreSQL client
│   │   │   ├── result.c/h
│   │   │   ├── settings.c/h
│   │   │   ├── status.c/h
│   │   │   ├── user.c/h
│   │   │   ├── util.c/h
│   │   │   ├── tests/
│   │   │   │   ├── test_parse.c
│   │   │   │   └── test_util.c
│   │   │   └── Makefile.am
│   │   │
│   │   ├── sql-server/                      # SQL Server-SPECIFIC IMPLEMENTATION
│   │   │   ├── argv.c/h
│   │   │   ├── bind.c/h                     # Parameter binding
│   │   │   ├── callback.c/h                 # TDS callbacks
│   │   │   ├── csv.c/h                      # CSV import/export
│   │   │   ├── export.c/h                   # BCP export functionality
│   │   │   ├── help.c
│   │   │   ├── import.c/h                   # BCP import functionality
│   │   │   ├── login.c/h                    # Login handling
│   │   │   ├── parse.c/h
│   │   │   ├── result.c/h
│   │   │   ├── settings.c/h
│   │   │   ├── sql_server.c/h               # Main SQL Server client
│   │   │   ├── status.c/h
│   │   │   ├── user.c/h
│   │   │   ├── tests/
│   │   │   │   ├── test_csv.c
│   │   │   │   └── test_parse.c
│   │   │   └── Makefile.am
│   │   │
│   │   └── common/
│   │       └── common/
│   │           └── clipboard.h              # Apache-licensed shared clipboard impl
│   │
│   ├── util/
│   │   └── generate-test-runner.pl          # Test framework generator
│   │
│   └── m4/                                  # Autotools macros
│
└── Associated Java JDBC Drivers:
    ├── /core/packages/kcm-mariadb-java-client/
    │   └── kcm-mariadb-java-client.spec     # MariaDB JDBC 2.7.12 (used by guacamole-client)
    │
    └── /core/packages/kcm-mssql-jdbc/
        └── kcm-mssql-jdbc.spec              # MSSQL JDBC 9.4.1 (used by guacamole-client)

/core/packages/kcm-libsybdb/
└── kcm-libsybdb.spec                        # FreeTDS 1.5.4 (TDS library for SQL Server)

/core/packages/kcm-guacamole-client/extra/
├── mysql.json                               # Protocol definition (configuration form)
├── postgres.json                            # Protocol definition (configuration form)
└── sql-server.json                          # Protocol definition (configuration form)
```

---

## 3. Build and Compilation Process

### 3.1 Build System Overview

**Build Tool**: Autotools (Autoconf/Automake)

#### Build Dependencies (from kcm-libguac-client-db.spec)

```
BuildRequires:
  - autoconf, automake, gcc, libtool, make        # Build tools
  - cairo-devel                                    # Graphics
  - CUnit-devel                                    # Unit testing
  - kcm-common-base                                # KCM base utilities
  - kcm-libguac-devel                              # Core Guacamole library
  - kcm-libguac-terminal-devel                     # Terminal emulation
  - kcm-libsybdb-devel                             # FreeTDS (SQL Server)
  - kcm-libedit-devel                              # Command-line editing
  - mariadb-devel                                  # MySQL client library
  - ncurses-devel                                  # Terminal control
  - postgresql-devel                               # PostgreSQL client library
```

### 3.2 Build Configuration (configure.ac)

```bash
AC_INIT([libguac-client-db], [1.6.0])

# Library checks:
AC_CHECK_LIB([guac], [guac_client_stream_png])          # Core protocol
AC_CHECK_LIB([guac-terminal], [guac_terminal_reset])    # Terminal emulation
AC_CHECK_LIB([ncurses], [initscr])                      # Terminal control
AC_CHECK_LIB([editkcm], [readline])                     # CLI editing (KCM variant)
AC_CHECK_LIB([pq], [PQlibVersion])                      # PostgreSQL
AC_CHECK_LIB([mysqlclient], [mysql_init])               # MySQL
AC_CHECK_LIB([sybdb], [dbsqlexec])                      # FreeTDS
AC_CHECK_LIB([cunit], [CU_run_test])                    # Unit tests
```

### 3.3 Compilation Process

**Command**:
```bash
cd libguac-client-db/
autoreconf -fi
./configure --disable-static \
    LDFLAGS="-L/usr/lib64/mysql -L/usr/lib64/postgres -L/usr/lib64/sql-server -L${_libdir}" \
    CPPFLAGS="-I${_includedir}"
make
```

**Result**: Three shared libraries (.so files)
- `libguac-client-mysql.so`
- `libguac-client-postgres.so`
- `libguac-client-sql-server.so`

### 3.4 Installation and Packaging

**Installation Phase**:
```bash
make install
```

**Installed Files**:
```
/usr/lib64/libguac-client-mysql.so{,.1}
/usr/lib64/libguac-client-postgres.so{,.1}
/usr/lib64/libguac-client-sql-server.so{,.1}

/usr/share/libguac-client-mysql/
/usr/share/libguac-client-postgres/
/usr/share/libguac-client-sql-server/
  └── NOTICE                                   # License information
  └── SBOMs/                                   # Software Bill of Materials
```

**RPM Package Names** (from .spec file):
- `kcm-libguac-client-mysql` (requires: kcm-guacd, kcm-libedit)
- `kcm-libguac-client-postgres` (requires: kcm-guacd, kcm-libedit)
- `kcm-libguac-client-sql-server` (requires: kcm-guacd, kcm-libedit)

---

## 4. Implementation Details - Plugin Architecture

### 4.1 Plugin Initialization Pattern

All three plugins follow the same entry point convention:

**File**: `{mysql|postgres|sql-server}.c`

```c
// Called by guacd when loading the plugin
int guac_client_init(guac_client* client) {
    
    // 1. Register connection arguments (from .json definitions)
    client->args = guac_db_combine_settings_args(
        DB_SPECIFIC_ARGS, DB_SPECIFIC_COUNT,
        SHARED_DB_ARGS, DB_ARGS_COUNT);
    
    // 2. Initialize database client
    guac_db_client* db_client = guac_db_init();
    
    // 3. Set database-specific query handler
    db_client->query_handler = guac_DBTYPE_query_handler;
    
    // 4. Allocate database-specific state
    guac_DBTYPE_client* specific = guac_mem_zalloc(sizeof(...));
    db_client->data = specific;
    client->data = db_client;
    
    // 5. Register handlers
    client->join_handler = guac_DBTYPE_user_join_handler;
    client->leave_handler = guac_DBTYPE_user_leave_handler;
    client->free_handler = guac_DBTYPE_client_free_handler;
    client->join_pending_handler = guac_db_join_pending_handler;
    
    // 6. Register argv callbacks
    guac_argv_register(GUAC_DB_ARGV_COLOR_SCHEME, guac_DBTYPE_argv_callback, ...);
    guac_argv_register(GUAC_DB_ARGV_FONT_NAME, guac_DBTYPE_argv_callback, ...);
    guac_argv_register(GUAC_DB_ARGV_FONT_SIZE, guac_DBTYPE_argv_callback, ...);
    
    return 0;
}
```

### 4.2 Shared Database Library (libguac-client-db)

The `db/` directory contains a shared library with common functionality:

**Purpose**: Reduce code duplication across MySQL, PostgreSQL, and SQL Server implementations

**Key Components**:

#### guac_db_client Structure (db/client.h)
```c
typedef struct guac_db_client {
    guac_db_settings* settings;
    pthread_t client_thread;
    int pseudoterminal_active;
    int guac_terminal_pty_fd;           // Guacamole-facing terminal
    int db_terminal_pty_fd;              // Database-facing terminal
    guac_terminal* term;                 // Terminal emulation
    guac_recording* recording;           // Session recording
    EditLine* editline;                  // Line editing
    void* data;                          // DB-specific state
    guac_db_query_handler *query_handler; // Query processing callback
    guac_db_command_clear_handler* clear_handler;
    guac_db_prompt_generator* prompt_generator;
    guac_db_export_data* export_data;
} guac_db_client;
```

#### Common Functionality (db/db/*.h/c files)

| Module | Purpose |
|--------|---------|
| `argv.c` | Argument parsing, settings combination |
| `client.c` | Input/output thread management, pseudoterminal setup |
| `clipboard.c` | Clipboard buffer management |
| `export.c` | Generic CSV export logic |
| `help.c` | CLI help system |
| `input.c` | User input handling from terminal |
| `parse.c` | Common query parsing utilities |
| `pipe.c` | Pipe communication abstractions |
| `result.c` | Result set display formatting |
| `settings.c` | Connection settings management |
| `status.c` | Status line display |
| `util.c` | Utility functions (memory, string handling) |

### 4.3 Database-Specific Client Structures

#### MySQL Client (src/mysql/mysql.h)
```c
typedef struct guac_mysql_client {
    guac_mysql_settings* settings;
    MYSQL* mysql;                        // MySQL connection handle
    guac_mysql_import_data* import_data; // CSV import state
} guac_mysql_client;
```

#### PostgreSQL Client (src/postgres/postgres.h)
```c
typedef struct guac_postgres_client {
    guac_postgres_settings* settings;
    PGconn* postgres;                      // PostgreSQL connection
    guac_postgres_export_data* export_data; // CSV export state
    guac_postgres_import_data* import_data; // CSV import state
} guac_postgres_client;
```

#### SQL Server Client (src/sql-server/sql_server.h)
```c
typedef struct guac_sql_server_client {
    guac_sql_server_settings* settings;
    DBPROCESS* db_process;                // FreeTDS database process
    guac_sql_server_import_data* import_data;
    regex_t go_matcher;                   // "GO" batch command matcher
    regex_t import_matcher;               // Import command matcher
} guac_sql_server_client;
```

### 4.4 Handler Functions

All plugins implement the same handler interface:

#### Join Handler
Called when a user connects to the database session:
```c
int guac_DBTYPE_user_join_handler(guac_user* user, int argc, char** argv)
```

#### Leave Handler  
Called when a user disconnects:
```c
int guac_DBTYPE_user_leave_handler(guac_user* user)
```

#### Free Handler
Called when the client is destroyed:
```c
int guac_DBTYPE_client_free_handler(guac_client* client)
```

#### Query Handler
Called to process SQL queries from the terminal:
```c
void guac_DBTYPE_query_handler(guac_user* user, char* command_buffer, int* command_length)
```

---

## 5. Connection Management, Authentication, and Configuration

### 5.1 Connection Arguments (from .json Protocol Definitions)

**Location**: `/core/packages/kcm-guacamole-client/extra/{mysql|postgres|sql-server}.json`

#### MySQL/PostgreSQL Arguments:
```json
{
  "name": "mysql",
  "connectionForms": [
    {
      "name": "network",
      "fields": [
        { "name": "hostname", "type": "TEXT" },
        { "name": "port", "type": "NUMERIC" }
      ]
    },
    {
      "name": "authentication",
      "fields": [
        { "name": "username", "type": "USERNAME" },
        { "name": "password", "type": "PASSWORD" }
      ]
    },
    {
      "name": "database",
      "fields": [
        { "name": "database", "type": "TEXT" },
        { "name": "disable-csv-export", "type": "BOOLEAN" },
        { "name": "disable-csv-import", "type": "BOOLEAN" }
      ]
    },
    // ... display, clipboard, typescript, recording, WoL settings
  ]
}
```

#### SQL Server Additional Arguments:
```json
{
  "name": "authentication",
  "fields": [
    { "name": "protocol-version", 
      "type": "ENUM", 
      "options": ["", "7.0", "7.1", "7.2", "7.3", "7.4", "8.0"] },
    { "name": "force-encryption", "type": "BOOLEAN" },
    { "name": "disable-cert-hostname-verification", "type": "BOOLEAN" },
    { "name": "ca-certificate", "type": "MULTILINE" }
  ]
}
```

### 5.2 Settings Structure (Enum-based Argument Indexing)

**MySQL Settings** (src/mysql/settings.h):
```c
enum MYSQL_ARGS_IDX {
    IDX_HOSTNAME,
    IDX_PORT,
    IDX_UNIX_SOCKET,
    IDX_USERNAME,
    IDX_PASSWORD,
    IDX_DATABASE,
    MYSQL_ARGS_COUNT
};

typedef struct guac_mysql_settings {
    char* hostname;
    int port;
    char* unix_socket;
    char* username;
    char* password;
    char* database;
} guac_mysql_settings;
```

**Common DB Settings** (src/db/db/settings.h):
```c
enum DB_ARGS_IDX {
    IDX_DISABLE_CSV_EXPORT,
    IDX_DISABLE_CSV_IMPORT,
    IDX_FONT_NAME,
    IDX_FONT_SIZE,
    IDX_COLOR_SCHEME,
    IDX_TYPESCRIPT_PATH,
    IDX_TYPESCRIPT_NAME,
    IDX_CREATE_TYPESCRIPT_PATH,
    IDX_TYPESCRIPT_WRITE_EXISTING,
    IDX_RECORDING_PATH,
    // ... more settings
    DB_ARGS_COUNT
};
```

### 5.3 Authentication Flow

1. **Configuration**: Credentials passed via Guacamole protocol handshake
2. **Parsing**: Settings parsed from connection arguments (argv)
3. **Connection Initialization**: Database-specific connection establishment
   - **MySQL**: `mysql_real_connect()`
   - **PostgreSQL**: `PQconnectdb()` or `PQconnectdbParams()`
   - **SQL Server**: FreeTDS login process with `DBSETLUSER()`, `DBSETLPWD()`
4. **Query Execution**: After authentication, queries can be submitted
5. **Session Recording**: All activities logged if recording enabled

### 5.4 Configuration Storage

Configuration is **stateless** - credentials are not stored by the plugins:
- Managed by the guacamole-client web application
- Stored in guacamole_client_configuration database (managed separately)
- Passed at connection time through Guacamole protocol

---

## 6. Role in guacd Integration

### 6.1 Plugin Loading Mechanism

**Location**: guacd daemon (separate package: kcm-guacamole-server)

**Flow**:
1. Client connects to guacd on TCP port 4822
2. Guacamole protocol handshake identifies connection type (e.g., "mysql")
3. Guacd searches for `libguac-client-{mysql|postgres|sql-server}.so`
4. Loads plugin dynamically using `dlopen()` / `dlsym()`
5. Calls `guac_client_init()` entry point
6. Routes user input to plugin, receives display output

### 6.2 Threading Model

Each database connection uses:
- **Main Thread**: guacd connection handler
- **Input Thread**: Reads Guacamole protocol from client, writes to pseudoterminal
- **Output Thread**: Reads from pseudoterminal, writes Guacamole protocol to client
- **Client Thread**: Processes queries from terminal input

**Synchronization**: Pthread mutexes protect:
- Socket operations
- Clipboard buffer access
- Recording file writes
- Connection state transitions

### 6.3 Data Flow Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  guacamole-client (Web Frontend)                            │
│  - User types SQL queries                                    │
│  - Sends Guacamole protocol messages (keyboard, mouse)       │
└────────────────┬────────────────────────────────────────────┘
                 │
                 │ Guacamole Protocol (TCP/WebSocket)
                 ↓
┌─────────────────────────────────────────────────────────────┐
│  guacd (guacamole-server daemon)                            │
│                                                              │
│  Connection Listener (port 4822)                            │
│         │                                                    │
│         ├─→ Protocol Detection (mysql/postgres/sql-server)  │
│         │                                                    │
│         └─→ Plugin Load (dlopen/dlsym guac_client_init)    │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ Loaded Plugin (libguac-client-DBTYPE.so)            │   │
│  │                                                       │   │
│  │  Input Thread ──→ Pseudoterminal ──→ Query Handler  │   │
│  │                                                       │   │
│  │  Output Thread ←─ DB Output ←─ DB Connection       │   │
│  │                    (MySQL/PgSQL/MSSQL)              │   │
│  │                                                       │   │
│  │  Handlers:                                           │   │
│  │  - join_handler, leave_handler, free_handler        │   │
│  │  - query_handler (parse and execute SQL)            │   │
│  │  - argv_callback (handle dynamic settings)          │   │
│  └──────────────────────────────────────────────────────┘   │
└────────────────┬────────────────────────────────────────────┘
                 │
                 │ Native DB Protocol (TCP)
                 ↓
┌─────────────────────────────────────────────────────────────┐
│  Database Server (MySQL, PostgreSQL, or SQL Server)         │
│  - Executes queries                                          │
│  - Returns result sets                                       │
└─────────────────────────────────────────────────────────────┘
```

---

## 7. Patch Files and KCM-Specific Modifications

### 7.1 Database-Related Patches in guacamole-client

**File**: `/core/packages/kcm-guacamole-client/patches/0015-KCM-446-Correct-SQL-syntax-errors-affecting-MySQL-Ma.patch`

**Issue**: KCM-446 - SQL syntax errors in JDBC mappers

**Changes**:
- MySQL: Fixed missing `AND` operator in DELETE query
- MySQL: Fixed typo `caesSensitive` → `caseSensitive` 
- SQL Server: Fixed XML tag `<hen>` → `<when>`
- SQL Server: Fixed missing `AND` in permission deletion query
- SQL Server: Fixed unclosed parenthesis in LOWER() function
- SQL Server: Fixed typo `LOOWER()` → `LOWER()`

**Database Drivers Updated**:
- MySQL JDBC: 8.3.0 → 9.3.0 (in Dockerfile)
- PostgreSQL JDBC: 42.7.2 → 42.7.7 (in Dockerfile)
- MSSQL JDBC: 9.4.1 (unchanged)

### 7.2 Keeper-Specific Modifications

All source files have **Keeper Security copyright header**:

```c
/*
 * Copyright (c) 2022 Keeper Security, Inc. All rights reserved.
 *
 * Unless otherwise agreed in writing, this software and any associated
 * documentation files (the "Software") may be used and distributed solely
 * in accordance with the Keeper Connection Manager EULA:
 *
 *     https://www.keepersecurity.com/termsofuse.html?t=k
 */
```

**KCM-Specific Features**:

1. **libedit Integration**: Uses `editkcm` (Keeper-modified libedit) for CLI
   - Better keyboard handling
   - Customized readline behavior
   - Command history management

2. **Terminal Recording**: 
   - Session typescripts (command timing and output)
   - Session recordings (Guacamole protocol playback)
   - Configurable paths and names

3. **Clipboard Management**:
   - Configurable buffer sizes (262KB, 1MB, 10MB)
   - Protected with mutexes for thread safety
   - Disable copy/paste options per-connection

4. **Wake-on-LAN (WoL)**:
   - Optional WoL packet sending before connection
   - Configurable MAC address, broadcast address, wait time

### 7.3 Version Information

**Current Versions**:
- libguac-client-db: 1.6.0
- kcm-libsybdb (FreeTDS): 1.5.4
- kcm-mariadb-java-client: 2.7.12
- kcm-mssql-jdbc: 9.4.1

**Changelog** (from .spec files):
- 1.6.0-5: Correct WoL wording (KCM-459)
- 1.6.0-4: Fix SQL command line artifacts (KCM-443)
- 1.6.0-3: Merge 2.20.1 release
- 1.6.0-2: Add MSSQL connection options (KCM-440)
- 1.6.0-1: Rebuild against latest upstream 1.6.0
- 1.5.5-8: Automatic SBOM generation (KCM-429)
- 1.5.5-7: Improve clipboard warnings (KCM-405)
- 1.5.5-6: Add clipboard limit configuration (KCM-405)
- 1.5.5-5: Ensure binary data escaping (KCM-399)

---

## 8. Database-Specific Implementation Details

### 8.1 MySQL/MariaDB Implementation

**Connection Library**: libmysqlclient (MariaDB Connector)

**Key Files**:
- `mysql.c` - Main initialization and query handling
- `export.c` - Uses `INTO LOCAL OUTFILE` for CSV export
- `import.c` - Loads CSV data with `LOAD DATA INFILE`
- `parse.c` - Parses MySQL queries for export detection
- `user.c` - Handles multi-user sessions

**Special Features**:
- Unix socket support for local connections
- Support for MariaDB and MySQL 5.7+
- Detection of `INTO LOCAL OUTFILE` for intercepting exports

**Settings**:
```c
IDX_HOSTNAME,
IDX_PORT,
IDX_UNIX_SOCKET,     // Unix socket for localhost connections
IDX_USERNAME,
IDX_PASSWORD,
IDX_DATABASE
```

### 8.2 PostgreSQL Implementation

**Connection Library**: libpq (PostgreSQL C API)

**Key Files**:
- `postgres.c` - Main initialization and query handling
- `export.c` - Uses `COPY TO` for CSV export
- `import.c` - Uses `\COPY` meta-command for import
- `meta.c` - Handles PostgreSQL meta-commands (\d, \l, \dt, etc.)
- `user.c` - Handles multi-user sessions
- `util.c` - Utility functions specific to PostgreSQL

**Special Features**:
- PostgreSQL meta-command support (\d, \dt, \di, \l, \du, etc.)
- COPY protocol for efficient data export
- Transaction tracking and error handling
- Support for connection parameter arrays

**Unique Meta-Commands**:
```
\d [OBJECT]        - Describe table/view
\dt [PATTERN]      - List tables
\di [PATTERN]      - List indexes
\l                 - List databases
\du [PATTERN]      - List users/roles
\dn [PATTERN]      - List schemas
```

### 8.3 Microsoft SQL Server Implementation

**Connection Library**: FreeTDS/libsybdb (TDS protocol)

**Key Files**:
- `sql_server.c` - Main initialization and query handling
- `export.c` - Uses BCP (Bulk Copy Protocol) for export
- `import.c` - Uses BCP for CSV import
- `csv.c` - CSV parsing and generation
- `login.c` - Login and encryption handling
- `parse.c` - Parses SQL Server queries
- `user.c` - Handles multi-user sessions
- `bind.c` - Parameter binding
- `callback.c` - TDS protocol callbacks

**Special Features**:
- TDS protocol versions: 7.0, 7.1, 7.2, 7.3, 7.4, 8.0
- Optional forced encryption
- Certificate verification control
- CA certificate specification
- GO batch command support
- Regex-based "GO" command detection
- CSV import with custom syntax

**TDS-Specific Settings**:
```c
protocol-version      // TDS version selection
force-encryption      // Require encrypted connection
disable-cert-hostname-verification  // Skip hostname verification
ca-certificate        // Custom CA certificate (PEM format)
```

**Import Syntax** (custom):
```sql
IMPORT "table_name" "[path/to/file.csv]"
```

---

## 9. Architecture of How Plugins Work with guacamole-client

### 9.1 guacamole-client Integration

**Location**: `/core/packages/kcm-guacamole-client/`

**Role**: Web-based frontend to guacd

**Database Connection Flow**:

1. **User Creates Connection** (Web UI):
   - Specifies "mysql", "postgres", or "sql-server" as protocol
   - Enters connection details (hostname, port, username, password, database)
   - Configures optional settings (font, colors, recording, WoL, CSV, etc.)

2. **guacamole-client Stores Configuration**:
   - Connection saved in relational database (MySQL or PostgreSQL)
   - Uses JDBC drivers for persistence
   - Encryption handled by guacamole-client

3. **User Launches Connection**:
   - Web app initiates WebSocket connection
   - Connects to guacd on TCP port 4822
   - Performs Guacamole protocol handshake
   - Sends connection arguments (hostname, port, credentials, options)

4. **Guacd Loads Plugin**:
   - Detects protocol type from handshake
   - Dynamically loads `libguac-client-{mysql|postgres|sql-server}.so`
   - Calls `guac_client_init()` with connection arguments

5. **Plugin Establishes DB Connection**:
   - Parses settings from argv
   - Connects to actual database using native client library
   - Authenticates with provided credentials
   - Selects initial database if specified

6. **Terminal Session Begins**:
   - Plugin creates pseudoterminal (PTY) pair
   - Displays database prompt (mysql>, postgres=>, 1>, etc.)
   - User types SQL queries

7. **Query Execution**:
   - User input captured from terminal
   - Query handler parses input
   - Detects special operations (export, import)
   - Executes query against database
   - Formats and displays results

8. **Session Recording**:
   - If enabled, records all commands and output
   - Timestamp information recorded for playback
   - CSV export captures result sets

### 9.2 guacamole-client to Database Plugin Communication

**Guacamole Protocol Messages**:

| Direction | Instruction | Data |
|-----------|-------------|------|
| Client → guacd | `key` | Keyboard input (pressed keys) |
| Client → guacd | `mouse` | Mouse events (not used for DB) |
| Client → guacd | `clipboard` | Clipboard content |
| guacd → Client | `char` | Terminal character output |
| guacd → Client | `copy` | Copy content to clipboard |
| guacd → Client | `file` | File transfer (recordings, typescripts) |

### 9.3 JDBC Drivers for guacamole-client

guacamole-client uses separate JDBC drivers to connect to the **same database** for:
- User authentication
- Connection configuration storage
- Audit logging
- Permission management

**Packages**:
- `kcm-mariadb-java-client` (2.7.12) - MySQL/MariaDB JDBC 4.2
  - Used by guacamole-client to store connection definitions
  
- `kcm-mssql-jdbc` (9.4.1) - MSSQL JDBC 4.2
  - Used by guacamole-client when using SQL Server backend
  
- PostgreSQL JDBC (via upstream) - Also available for PostgreSQL backend

**Relationship**:
```
guacamole-client                    Database Plugins (guacd)
  │                                         │
  │ JDBC to MySQL                          │
  │ (guacamole schema)                     │ User's MySQL connection
  └─→ MySQL Configuration DB       guacd → MySQL Target DB
                                            │
  │                                        │
  │ JDBC to PostgreSQL                     │
  │ (guacamole schema)              guacd → PostgreSQL Target DB
  └─→ PostgreSQL Configuration DB          │
                                           │
  │                                        │
  │ JDBC to SQL Server                     │
  │ (guacamole schema)              guacd → SQL Server Target DB
  └─→ SQL Server Configuration DB          │
```

---

## 10. Summary of Key Features

### 10.1 Supported Operations

All three plugins support:
- **Query Execution**: Full SQL support for each database
- **Result Display**: Terminal-based table formatting with pagination
- **CSV Export**: Export result sets as CSV files
- **CSV Import**: Import data from CSV files
- **Session Recording**: Complete session playback
- **Session Typewriting**: Command timing and output capture
- **Clipboard**: Share data between terminal and web browser
- **Multi-user**: Multiple users can share same connection
- **Terminal Control**: ANSI color codes, scrollback buffer
- **Wake-on-LAN**: Optional machine wake-up before connection

### 10.2 Database-Specific Differences

| Feature | MySQL | PostgreSQL | SQL Server |
|---------|-------|-----------|-----------|
| Export Method | `INTO LOCAL OUTFILE` | `COPY TO` | BCP |
| Import Method | `LOAD DATA INFILE` | `\COPY` | BCP |
| Meta-Commands | None | Yes (\d, \l, etc.) | GO batching |
| Authentication | User/Pass | User/Pass | User/Pass + TDS version |
| Connection Type | TCP or Unix Socket | TCP only | TCP (TDS) |
| Encryption | SSL/TLS optional | SSL/TLS optional | Forced optional |
| Driver Library | libmysqlclient | libpq | libsybdb (FreeTDS) |

### 10.3 Performance Considerations

- **Shared Library** (libguac-client-db): Reduces memory footprint, enables code sharing
- **Pseudoterminals**: Efficient I/O buffering between guacd and plugins
- **Threading**: Per-connection threads prevent blocking other connections
- **Mutex Locks**: Minimal contention for shared resources
- **Buffer Management**: Configurable clipboard sizes prevent memory exhaustion

---

## 11. File Locations Summary

### 11.1 Complete File Path Reference

| Component | Location |
|-----------|----------|
| **RPM Spec File** | `/core/packages/kcm-libguac-client-db/kcm-libguac-client-db.spec` |
| **Build Config** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/configure.ac` |
| **Build Files** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/Makefile.am` and subdirectories |
| **Shared DB Library** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/db/` |
| **MySQL Plugin Source** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/mysql/` |
| **PostgreSQL Plugin Source** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/postgres/` |
| **SQL Server Plugin Source** | `/core/packages/kcm-libguac-client-db/extra/libguac-client-db/src/sql-server/` |
| **FreeTDS Package** | `/core/packages/kcm-libsybdb/kcm-libsybdb.spec` |
| **MariaDB JDBC** | `/core/packages/kcm-mariadb-java-client/kcm-mariadb-java-client.spec` |
| **MSSQL JDBC** | `/core/packages/kcm-mssql-jdbc/kcm-mssql-jdbc.spec` |
| **Protocol Definitions** | `/core/packages/kcm-guacamole-client/extra/{mysql|postgres|sql-server}.json` |
| **Database Patches** | `/core/packages/kcm-guacamole-client/patches/0015-KCM-446-*.patch` |
| **Docker MySQL DB** | `/docker/images/guacamole-db-mysql/` |
| **Docker PostgreSQL DB** | `/docker/images/guacamole-db-postgres/` |

---

## Conclusion

The Keeper Connection Manager database plugins represent a comprehensive, production-grade implementation of interactive database terminal access through a web-based interface. The modular architecture, with a shared library base and database-specific implementations, demonstrates excellent software engineering practices. The plugins integrate seamlessly with Apache Guacamole's threading model and protocol framework while providing security features like session recording, clipboard control, and configurable data export. The three supported databases (MySQL, PostgreSQL, and SQL Server) are implemented with appropriate native client libraries and protocols (MySQL C API, libpq, and FreeTDS respectively), ensuring authentic, performant connections to production database systems.


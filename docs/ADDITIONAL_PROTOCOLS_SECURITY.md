# Additional Protocol Support & Security Analysis

## Security-First Protocol Expansion

Since security is the #1 priority, let's analyze additional protocols with security implications.

---

## Additional Database Protocols

### 1. Oracle Database

**Use Case:** Enterprise databases
**Port:** 1521 (default)
**Security Concerns:**
- Credential exposure (username/password in params)
- SQL injection if query parameters not sanitized
- Privilege escalation via PL/SQL

**Handler Implementation:**
```rust
pub struct OracleHandler {
    config: OracleConfig,
}

// Use oracle crate (Pure Rust Oracle driver)
[dependencies]
oracle = "0.6"

impl ProtocolHandler for OracleHandler {
    fn name(&self) -> &str { "oracle" }

    async fn connect(...) -> Result<()> {
        // Security: Validate inputs
        let hostname = validate_hostname(params.get("hostname"))?;
        let username = sanitize_username(params.get("username"))?;

        // Connect with proper encryption (SSL/TLS)
        let conn = oracle::Connection::connect(
            username,
            password,
            &format!("{}:{}/ORCL", hostname, port),
        )?;

        // Security: Set session parameters
        conn.execute("ALTER SESSION SET SQL_TRACE = FALSE", &[])?;  // No logging
        conn.execute("ALTER SESSION SET CURRENT_SCHEMA = ?", &[&username])?;  // Restrict schema

        // Execute queries with prepared statements (prevent SQL injection)
        let mut stmt = conn.prepare("SELECT * FROM table WHERE id = :1")?;
        stmt.execute(&[&user_input])?;  // Parameterized - safe
    }
}
```

**Security Measures:**
- ✅ Parameterized queries (prevent SQL injection)
- ✅ TLS encryption for connection
- ✅ Schema restrictions
- ✅ No query logging (prevent credential leaks)
- ✅ Connection timeout limits

### 2. MariaDB

**Use Case:** MySQL-compatible, open source
**Port:** 3306
**Handler:** Can reuse MySQL handler (wire protocol compatible)

```rust
// MariaDB uses same protocol as MySQL
pub type MariaDbHandler = MySqlHandler;
```

**Security:** Same as MySQL (already covered)

### 3. Apache Cassandra

**Use Case:** Distributed NoSQL
**Port:** 9042 (CQL native protocol)
**Security Concerns:**
- CQL injection
- Wide-column access control
- Data exposure across keyspaces

**Handler Implementation:**
```rust
[dependencies]
scylla = "0.13"  // Pure Rust Cassandra driver

pub struct CassandraHandler;

impl ProtocolHandler for CassandraHandler {
    fn name(&self) -> &str { "cassandra" }

    async fn connect(...) -> Result<()> {
        use scylla::SessionBuilder;

        // Security: TLS required
        let session = SessionBuilder::new()
            .known_node(hostname)
            .user(username, password)
            .use_keyspace(keyspace, false)
            .build()
            .await?;

        // Execute with prepared statements
        let prepared = session.prepare("SELECT * FROM table WHERE key = ?").await?;
        session.execute(&prepared, (user_input,)).await?;
    }
}
```

**Security Measures:**
- ✅ TLS enforcement
- ✅ Keyspace isolation
- ✅ Prepared statements
- ✅ Authentication required

### 4. MongoDB

**Use Case:** Document database
**Port:** 27017
**Security Concerns:**
- NoSQL injection
- Authentication bypass
- Database enumeration

**Handler Implementation:**
```rust
[dependencies]
mongodb = "3.1"

pub struct MongoDbHandler;

impl ProtocolHandler for MongoDbHandler {
    fn name(&self) -> &str { "mongodb" }

    async fn connect(...) -> Result<()> {
        use mongodb::Client;

        // Security: Auth + TLS
        let client = Client::with_uri_str(&format!(
            "mongodb://{}:{}@{}:{}/{}?tls=true&tlsAllowInvalidCertificates=false",
            username, password, hostname, port, database
        )).await?;

        // Use BSON for queries (type-safe)
        let collection = client.database(db).collection(coll);
        collection.find_one(doc! { "_id": user_input }, None).await?;
    }
}
```

**Security:**
- ✅ TLS required
- ✅ Strong authentication
- ✅ BSON prevents injection
- ✅ Database-level isolation

### 5. Redis

**Use Case:** Cache, session storage
**Port:** 6379
**Security Concerns:**
- No authentication by default
- Command injection
- Data exposure

**Handler Implementation:**
```rust
[dependencies]
redis = { version = "0.27", features = ["tokio-comp", "connection-manager"] }

pub struct RedisHandler;

impl ProtocolHandler for RedisHandler {
    fn name(&self) -> &str { "redis" }

    async fn connect(...) -> Result<()> {
        use redis::aio::ConnectionManager;

        // Security: Require password, use TLS
        let client = redis::Client::open(format!(
            "rediss://{}:{}@{}:{}/{}",  // rediss = TLS
            username, password, hostname, port, database
        ))?;

        let mut conn = ConnectionManager::new(client).await?;

        // ACL restrictions (Redis 6+)
        redis::cmd("ACL")
            .arg("SETUSER")
            .arg(username)
            .arg("on")
            .arg(&format!("~{}", keyspace_pattern))  // Restrict key access
            .query_async(&mut conn).await?;
    }
}
```

**Security:**
- ✅ TLS required (rediss://)
- ✅ Authentication mandatory
- ✅ ACL restrictions (Redis 6+)
- ✅ Keyspace isolation

### 6. Elasticsearch

**Use Case:** Search, analytics
**Port:** 9200
**Security Concerns:**
- Query DSL injection
- Index access control
- Data exfiltration

```rust
[dependencies]
elasticsearch = "8.15"

pub struct ElasticsearchHandler;

impl ProtocolHandler for ElasticsearchHandler {
    fn name(&self) -> &str { "elasticsearch" }

    async fn connect(...) -> Result<()> {
        use elasticsearch::{Elasticsearch, http::transport::Transport};

        // Security: HTTPS + auth
        let transport = Transport::single_node(&format!(
            "https://{}:{}@{}:{}",
            username, password, hostname, port
        ))?;

        let client = Elasticsearch::new(transport);

        // Restrict to specific index
        client.search(elasticsearch::SearchParts::Index(&[allowed_index]))
            .body(query_json)
            .send().await?;
    }
}
```

---

## Remote Administration Protocols

### 7. PowerShell Remoting (WinRM)

**Use Case:** Windows administration
**Port:** 5985 (HTTP), 5986 (HTTPS)
**Security Concerns:**
- Credential theft
- Command injection
- Lateral movement
- Privilege escalation

**Handler Implementation:**
```rust
pub struct PowerShellHandler {
    config: PowerShellConfig,
}

#[derive(Debug, Clone)]
pub struct PowerShellConfig {
    pub default_port: u16,  // 5986 (HTTPS)
    pub require_https: bool,  // Default: true
    pub execute_policy: ExecutePolicy,
}

pub enum ExecutePolicy {
    Restricted,      // No scripts
    AllSigned,       // Only signed scripts
    RemoteSigned,    // Local unsigned, remote signed
    Unrestricted,    // Dangerous!
}

impl ProtocolHandler for PowerShellHandler {
    fn name(&self) -> &str { "powershell" }

    async fn connect(...) -> Result<()> {
        // Security: HTTPS only in production
        if self.config.require_https && port != 5986 {
            return Err(HandlerError::SecurityViolation(
                "PowerShell remoting requires HTTPS (port 5986)".to_string()
            ));
        }

        // Use WinRM protocol (HTTP/HTTPS + SOAP)
        // TODO: Need WinRM Rust client (or call PowerShell via SSH)

        // Alternative: Use SSH to Windows and invoke PowerShell
        let ssh = SshHandler::new(config);
        ssh.connect_with_shell("powershell.exe", params).await?;

        // Security controls:
        // - Audit all commands
        // - Restrict execution policy
        // - No script uploads
        // - Session recording mandatory
    }
}
```

**Security Measures:**
- ✅ HTTPS required (TLS)
- ✅ Kerberos authentication
- ✅ Restricted execution policy
- ✅ Command auditing
- ✅ Session recording
- ✅ No script uploads
- ⚠️ HIGH RISK - requires strong controls

### 8. Kubernetes Pod Exec

**Already in ConversationType!** Just need to implement.

**Use Case:** Container administration
**Protocol:** WebSocket to Kubernetes API
**Security Concerns:**
- Cluster access
- Privilege escalation
- Secret exposure
- Container breakout

**Handler Implementation:**
```rust
[dependencies]
kube = { version = "0.95", features = ["runtime", "ws"] }

pub struct KubernetesHandler;

impl ProtocolHandler for KubernetesHandler {
    fn name(&self) -> &str { "kubernetes" }

    async fn connect(...) -> Result<()> {
        use kube::{Client, api::Api};
        use kube::api::AttachParams;

        // Security: Validate kubeconfig, verify RBAC
        let client = Client::try_default().await?;

        // Verify user has exec permission
        verify_rbac_permission(&client, namespace, pod, "exec").await?;

        // Attach to pod with restricted capabilities
        let pods: Api<Pod> = Api::namespaced(client, namespace);
        let ap = AttachParams {
            container: Some(container.clone()),
            tty: true,
            stdin: true,
            stdout: true,
            stderr: true,
            ..Default::default()
        };

        let mut attached = pods.exec(pod, vec!["sh"], &ap).await?;

        // Security: Record all commands
        let mut recorder = AsciicastRecorder::new(..=)?;

        // Terminal I/O with recording
        loop {
            tokio::select! {
                data = attached.stdout() => {
                    recorder.record_output(&data)?;  // Audit log
                    // ...
                }
            }
        }
    }
}
```

**Security Measures:**
- ✅ RBAC verification
- ✅ Namespace isolation
- ✅ kubeconfig validation
- ✅ Mandatory session recording
- ✅ No privilege escalation
- ⚠️ CRITICAL - audit all actions

### 9. Docker Exec

**Use Case:** Container access
**Protocol:** Docker API (Unix socket or HTTPS)

```rust
[dependencies]
bollard = "0.17"  // Docker API client

pub struct DockerHandler;

impl ProtocolHandler for DockerHandler {
    fn name(&self) -> &str { "docker" }

    async fn connect(...) -> Result<()> {
        use bollard::Docker;
        use bollard::exec::{CreateExecOptions, StartExecResults};

        // Security: Verify Docker socket permissions
        let docker = Docker::connect_with_socket_defaults()?;

        // Verify user has permission for this container
        verify_docker_permission(container_id).await?;

        // Create exec instance
        let exec = docker.create_exec(
            container_id,
            CreateExecOptions {
                cmd: Some(vec!["sh"]),
                attach_stdout: Some(true),
                attach_stdin: Some(true),
                attach_stderr: Some(true),
                tty: Some(true),
                ..Default::default()
            },
        ).await?;

        // Start with recording
        let mut recorder = AsciicastRecorder::new(..=)?;

        match docker.start_exec(&exec.id, None).await? {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(msg) = output.next().await {
                    recorder.record_output(&msg)?;
                    // Forward to client
                }
            }
            _ => {}
        }
    }
}
```

**Security:**
- ✅ Socket permission verification
- ✅ Container isolation
- ✅ No privileged containers
- ✅ Session recording
- ⚠️ RISKY - Docker socket = root access

---

## Secure Protocol Priority List

### Tier 1: Low Risk (Implement These)
1. ✅ **SSH** - Already done, standard security
2. ✅ **Telnet** - Already done (warn: unencrypted)
3. ✅ **MySQL** - Already done
4. ✅ **PostgreSQL** - Already done
5. ✅ **SQL Server** - Already done
6. **MariaDB** - Reuse MySQL handler
7. **MongoDB** - NoSQL, well-understood
8. **Redis** - Simple, can enforce TLS
9. **Elasticsearch** - Search, limited risk

### Tier 2: Medium Risk (Careful Implementation)
10. ✅ **RDP** - Already done foundation
11. ✅ **VNC** - Already done foundation
12. **Oracle** - Enterprise DB, good security model
13. **Cassandra** - Distributed DB, TLS support

### Tier 3: High Risk (Extreme Caution)
14. ✅ **RBI/HTTP** - Browser = attack surface, but isolated
15. **PowerShell** - Command execution = dangerous
16. **Kubernetes exec** - Cluster access = critical
17. **Docker exec** - Container access = risky

**Recommendation:** Implement Tier 1 fully, Tier 2 with security review, Tier 3 with mandatory auditing.

---

## Security Framework for All Handlers

### Mandatory Security Controls

```rust
pub struct SecurityPolicy {
    // Authentication
    pub require_tls: bool,              // Enforce encrypted connections
    pub min_password_length: usize,     // Minimum 12 characters
    pub allow_password_auth: bool,      // vs key-based only
    pub mfa_required: bool,             // Require 2FA

    // Session controls
    pub max_session_duration: Duration, // Default: 1 hour
    pub idle_timeout: Duration,         // Default: 15 minutes
    pub concurrent_limit: usize,        // Max sessions per user

    // Audit
    pub mandatory_recording: bool,      // Default: true
    pub audit_log_path: PathBuf,
    pub record_keystrokes: bool,        // Privacy vs security

    // Resource limits
    pub max_memory_mb: usize,
    pub max_cpu_percent: u32,
    pub network_egress_limit_mbps: u32,

    // Protocol restrictions
    pub allowed_protocols: HashSet<String>,
    pub protocol_specific_rules: HashMap<String, ProtocolRules>,
}

pub struct ProtocolRules {
    // For databases
    pub max_query_size: usize,
    pub disallow_dangerous_commands: bool,  // DROP, DELETE, TRUNCATE
    pub read_only: bool,

    // For shell access
    pub disallow_sudo: bool,
    pub disallow_su: bool,
    pub command_whitelist: Option<Vec<String>>,
    pub command_blacklist: Vec<String>,

    // For RBI
    pub url_whitelist: Option<Vec<String>>,
    pub url_blacklist: Vec<String>,
    pub block_downloads: bool,
    pub block_uploads: bool,
}
```

### Handler Security Wrapper

```rust
pub struct SecureProtocolHandler<H: ProtocolHandler> {
    inner: H,
    policy: SecurityPolicy,
    audit_logger: AuditLogger,
}

impl<H: ProtocolHandler> SecureProtocolHandler<H> {
    async fn connect(...) -> Result<()> {
        // 1. Pre-connect validation
        self.validate_params(&params)?;
        self.check_rate_limit()?;
        self.verify_user_permission()?;

        // 2. Start audit log
        let session_id = uuid::Uuid::new_v4();
        self.audit_logger.log_connection_start(session_id, &params)?;

        // 3. Start session recording if required
        let mut recorder = if self.policy.mandatory_recording {
            Some(AsciicastRecorder::new(..=)?)
        } else {
            None
        };

        // 4. Set up session timeout
        let timeout = tokio::time::sleep(self.policy.max_session_duration);

        // 5. Connect with monitoring
        tokio::select! {
            result = self.inner.connect(params, to_client, from_client) => {
                // Session completed normally
                self.audit_logger.log_connection_end(session_id, result)?;
                result
            }
            _ = timeout => {
                // Session timeout
                self.audit_logger.log_timeout(session_id)?;
                Err(HandlerError::Timeout("Session exceeded max duration".to_string()))
            }
        }
    }

    fn validate_params(&self, params: &HashMap<String, String>) -> Result<()> {
        // Check for SQL injection patterns
        if let Some(query) = params.get("initial_query") {
            if contains_sql_injection(query) {
                return Err(HandlerError::SecurityViolation(
                    "Potential SQL injection detected".to_string()
                ));
            }
        }

        // Validate hostname (no SSRF)
        if let Some(hostname) = params.get("hostname") {
            if is_internal_ip(hostname) && !self.policy.allow_internal_ips {
                return Err(HandlerError::SecurityViolation(
                    "Internal IP access not allowed".to_string()
                ));
            }
        }

        Ok(())
    }
}
```

---

## Additional Useful Protocols

### 10. SFTP (Secure File Transfer)

**Security:** Good (uses SSH)

```rust
pub struct SftpHandler;

impl ProtocolHandler for SftpHandler {
    fn name(&self) -> &str { "sftp" }

    async fn connect(...) -> Result<()> {
        // Reuse SSH handler for connection
        let ssh_session = establish_ssh(...).await?;

        // Open SFTP channel
        let sftp = ssh_session.sftp().await?;

        // File browser UI (render as spreadsheet-like grid)
        let files = sftp.readdir(path).await?;
        render_file_browser(files)?;

        // Security: Restrict to home directory
        sftp.chroot(home_dir)?;
    }
}
```

### 11. WinRM (Windows Remote Management)

**Security:** Medium risk (like PowerShell)

```rust
pub struct WinRmHandler;

impl ProtocolHandler for WinRmHandler {
    fn name(&self) -> &str { "winrm" }

    // Similar to PowerShell but uses WinRM protocol
}
```

### 12. Serial Console

**Use Case:** Hardware/embedded systems

```rust
pub struct SerialHandler;

impl ProtocolHandler for SerialHandler {
    fn name(&self) -> &str { "serial" }

    async fn connect(...) -> Result<()> {
        use tokio_serial::SerialStream;

        let port = params.get("device").unwrap_or("/dev/ttyUSB0");
        let baud_rate = params.get("baud").and_then(|b| b.parse().ok()).unwrap_or(115200);

        let mut serial = SerialStream::open(&SerialPortBuilder::new(port, baud_rate))?;

        // Terminal emulation for serial console
        // Security: Read-only mode option
    }
}
```

---

## Security Recommendations by Protocol

### Critical Security Protocols (Require Extra Controls):

```rust
pub fn get_security_level(protocol: &str) -> SecurityLevel {
    match protocol {
        // Low risk
        "ssh" | "telnet" => SecurityLevel::Low,

        // Medium risk
        "rdp" | "vnc" | "mysql" | "postgresql" | "sqlserver" |
        "mongodb" | "redis" | "cassandra" | "elasticsearch" => SecurityLevel::Medium,

        // High risk - command execution
        "powershell" | "winrm" | "kubernetes" | "docker" => SecurityLevel::High,

        // Critical risk - unrestricted access
        "http" => SecurityLevel::Critical,  // RBI - web = anything

        _ => SecurityLevel::Unknown,
    }
}

pub enum SecurityLevel {
    Low,       // Standard controls
    Medium,    // + TLS required, query validation
    High,      // + Command auditing, approval workflow
    Critical,  // + Sandboxing, resource limits, URL filtering
    Unknown,   // Deny by default
}
```

### Mandatory Controls by Level

**All Protocols:**
- Session recording (asciicast)
- Timeout limits
- Authentication required
- Audit logging

**Medium+:**
- TLS encryption required
- Input validation
- Prepared statements (databases)

**High:**
- Command auditing
- Approval workflow (optional)
- No dangerous commands (sudo, rm -rf, DROP TABLE)
- Restricted access

**Critical:**
- Resource limits (cgroups)
- Sandboxing
- URL filtering (RBI)
- Network egress monitoring
- Mandatory recording

---

## Recommended Protocol Additions

### Priority 1 (Add Now):
1. **MariaDB** - Easy (reuse MySQL)
2. **MongoDB** - Common, good security
3. **Redis** - Simple, enforce TLS
4. **Oracle** - Enterprise demand

### Priority 2 (Add Soon):
5. **Cassandra** - Distributed DB
6. **Elasticsearch** - Search
7. **SFTP** - File transfer (reuse SSH)

### Priority 3 (Consider):
8. **Kubernetes exec** - Already in enum, high demand
9. **PowerShell** - Windows admin (HIGH RISK)
10. **WinRM** - Windows management

### Do NOT Add (Too Risky):
- ❌ Docker exec (root equivalent)
- ❌ Unrestricted PowerShell
- ❌ Any protocol without TLS option

---

## Implementation Plan

### Phase 1: Low-Risk Databases (Now)
```rust
// Add to workspace
crates/guacr-database/src/
├── mysql.rs      ✓ (done)
├── postgresql.rs ✓ (done)
├── sqlserver.rs  ✓ (done)
├── mariadb.rs    ← Add (reuse MySQL)
├── mongodb.rs    ← Add (30 mins)
├── redis.rs      ← Add (30 mins)
└── oracle.rs     ← Add (2 hours)
```

### Phase 2: Additional DB (Next)
- Cassandra (2 hours)
- Elasticsearch (2 hours)

### Phase 3: Admin Protocols (Careful!)
- Kubernetes exec (4 hours + security review)
- SFTP (2 hours - reuse SSH)
- PowerShell (4 hours + strict controls)

---

## Security Audit Checklist

For each protocol handler:

- [ ] TLS encryption available?
- [ ] Authentication required?
- [ ] Input validation implemented?
- [ ] SQL/Command injection prevented?
- [ ] Session recording enabled?
- [ ] Resource limits enforced?
- [ ] Timeout configured?
- [ ] Audit logging present?
- [ ] SSRF protection (hostname validation)?
- [ ] Privilege escalation prevented?
- [ ] Error messages don't leak info?
- [ ] Credentials never logged?

---

## RECOMMENDATION

**Add these protocols NOW (30 min - 2 hours each):**
1. MariaDB (trivial - alias MySQL)
2. MongoDB (medium effort)
3. Redis (medium effort)
4. Oracle (more effort, but high demand)

**Add with SECURITY REVIEW:**
5. Kubernetes exec (high risk, but needed)
6. SFTP (low risk)

**DO NOT ADD without extreme controls:**
- Docker exec
- Unrestricted PowerShell/WinRM

All handlers wrap in `SecureProtocolHandler` with mandatory recording, timeouts, and validation.

Want me to implement MariaDB, MongoDB, Redis, and Oracle handlers now?

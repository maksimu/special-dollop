# Oracle Database Connection Setup

The Oracle database handler supports two modes:

1. **Real Mode** - Connects to actual Oracle databases (requires Oracle Instant Client)
2. **Simulation Mode** - Demo mode for testing without Oracle client (default)

## Quick Start

### Check Current Mode

The handler automatically detects which mode to use based on environment variables:

```bash
# Check if Oracle client is configured
echo $ORACLE_HOME
echo $OCI_LIB_DIR

# If neither is set, the handler runs in simulation mode
```

## Enabling Real Oracle Connections

### Step 1: Download Oracle Instant Client

1. Go to [Oracle Instant Client Downloads](https://www.oracle.com/database/technologies/instant-client/downloads.html)
2. Select your platform (Linux, macOS, Windows)
3. Download "Basic" or "Basic Light" package
4. **Accept Oracle's OTN License** (required)

### Step 2: Install the Client

#### Linux (Debian/Ubuntu)
```bash
# Extract to /opt/oracle
sudo mkdir -p /opt/oracle
sudo unzip instantclient-basic-linux.x64-21.*.zip -d /opt/oracle

# Create symlinks
cd /opt/oracle/instantclient_21_*
sudo ln -s libclntsh.so.21.1 libclntsh.so
sudo ln -s libocci.so.21.1 libocci.so

# Configure library path
echo "/opt/oracle/instantclient_21_*" | sudo tee /etc/ld.so.conf.d/oracle-instantclient.conf
sudo ldconfig
```

#### macOS
```bash
# Extract to /opt/oracle
sudo mkdir -p /opt/oracle
sudo unzip instantclient-basic-macos.x64-19.*.zip -d /opt/oracle

# The .dylib files should be in /opt/oracle/instantclient_19_*
```

#### Windows
1. Extract to `C:\oracle\instantclient_21_*`
2. Add to PATH: `C:\oracle\instantclient_21_*`

### Step 3: Set Environment Variables

Add to your shell profile (`~/.bashrc`, `~/.zshrc`, etc.):

```bash
# Option 1: ORACLE_HOME (recommended)
export ORACLE_HOME=/opt/oracle/instantclient_21_1

# Option 2: OCI_LIB_DIR (alternative)
export OCI_LIB_DIR=/opt/oracle/instantclient_21_1

# For macOS, also set DYLD_LIBRARY_PATH
export DYLD_LIBRARY_PATH=$ORACLE_HOME:$DYLD_LIBRARY_PATH

# For Linux, also set LD_LIBRARY_PATH (if not using ldconfig)
export LD_LIBRARY_PATH=$ORACLE_HOME:$LD_LIBRARY_PATH
```

### Step 4: Verify Installation

```bash
# Reload shell
source ~/.bashrc  # or ~/.zshrc

# Verify environment
echo $ORACLE_HOME

# Test with the handler - it should show "Connected to Oracle Database"
# instead of "SIMULATION MODE"
```

## Docker/Kubernetes Deployment

For containerized deployments:

```dockerfile
# Dockerfile example
FROM your-base-image

# Copy Oracle Instant Client (you must download separately)
COPY instantclient_21_1/ /opt/oracle/instantclient_21_1/

# Set environment
ENV ORACLE_HOME=/opt/oracle/instantclient_21_1
ENV LD_LIBRARY_PATH=/opt/oracle/instantclient_21_1

# Your app
COPY your-app /app
CMD ["/app/your-app"]
```

**Important**: Do NOT include Oracle Instant Client in public Docker images. Users must:
1. Download it themselves from Oracle
2. Accept Oracle's OTN License
3. Mount or copy it into their container

### Kubernetes ConfigMap Example

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: oracle-config
data:
  ORACLE_HOME: "/opt/oracle/instantclient_21_1"
```

## Licensing

### Oracle Instant Client License

The Oracle Instant Client is provided under the **Oracle Technology Network (OTN) License**:

- ✅ **Free to use** for development and production
- ✅ **Free to redistribute** with your application
- ⚠️ **Must accept Oracle's terms** before downloading
- ⚠️ **Cannot be bundled** in open-source distributions without user consent

### Our Code License

The `guacr-database` Oracle handler code is MIT/Apache-2.0 licensed. Only the Oracle Instant Client binary has OTN restrictions.

## Troubleshooting

### "SIMULATION MODE" showing when Oracle is installed

Check these:
1. Environment variable is set: `echo $ORACLE_HOME`
2. Library files exist: `ls $ORACLE_HOME/*.so` (Linux) or `*.dylib` (macOS)
3. Library path is configured: `echo $LD_LIBRARY_PATH` or `echo $DYLD_LIBRARY_PATH`

### Connection errors

Common issues:
- **ORA-12154**: TNS name not found - Check service name
- **ORA-12541**: No listener - Check hostname/port
- **ORA-01017**: Invalid credentials - Check username/password
- **ORA-28000**: Account locked - Contact DBA

### macOS SIP issues

If macOS blocks the Oracle libraries:
```bash
# Sign the libraries (requires dev tools)
codesign -s - $ORACLE_HOME/libclntsh.dylib
```

## Connection Parameters

When connecting via guacr, provide these parameters:

| Parameter | Description | Default |
|-----------|-------------|---------|
| `hostname` | Oracle server hostname | (required) |
| `port` | Oracle listener port | 1521 |
| `username` | Database username | (required) |
| `password` | Database password | (required) |
| `service` | Service name or SID | ORCL |
| `read-only` | Enable read-only mode | false |
| `disable-csv-export` | Disable CSV export | false |
| `disable-csv-import` | Disable CSV import | false |

## Example Connection

```
hostname: oracle.example.com
port: 1521
username: myuser
password: mypassword
service: PRODDB
```

The handler will display either:
- `Connected to Oracle Database` (real mode)
- `Oracle Database Handler - SIMULATION MODE` (no client installed)

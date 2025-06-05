# TURN Server Authentication Usage

This TURN server is now configured with authentication using `use-auth-secret` and `static-auth-secret`.

## Configuration

The authentication is configured with these Terraform variables:

- `turn_auth_secret`: The shared secret used for authentication (default: "your-secure-turn-secret-123")
- `domain_name_prefix`: Prefix for the domain name (default: "rustunnel-test")

## How to Use the Authentication

### 1. Setting the Authentication Secret

You can set a custom authentication secret in several ways:

**Option A: Using terraform.tfvars file**
```hcl
turn_auth_secret = "your-super-secure-secret-here"
domain_name_prefix = "my-custom-prefix"  # Optional, defaults to "rustunnel-test"
```

**Option B: Environment variable**
```bash
export TF_VAR_turn_auth_secret="your-super-secure-secret-here"
```

**Option C: Command line**
```bash
terraform apply -var="turn_auth_secret=your-super-secure-secret-here"
```

### 2. Getting the Server Information

After deployment, you can get the server details:

```bash
# Get all outputs
terraform output

# Get specific values
terraform output turn_server_hostname
terraform output -raw turn_server_auth_secret
```

### 3. Using in Client Code

When using this TURN server in your WebRTC application, you'll need to generate temporary credentials using the shared secret.

#### Bash Example (Quick Testing)

**Unified Script (Recommended):**
```bash
# Generate 48h credentials for current user (auto-detects server/secret)
./test_turn_credentials.sh

# Generate credentials for specific user with custom expiration
./test_turn_credentials.sh -u alice -t 24  # 24h for alice

# Generate and test the server
./test_turn_credentials.sh --test

# Manual server/secret
./test_turn_credentials.sh -s "your-secret" -h "your-server.com"

# See all options
./test_turn_credentials.sh --help
```

**Manual Script:**
```bash
#!/bin/bash

# Configuration
TURN_SECRET="your-super-secure-secret-here"  # Get this from: terraform output -raw turn_server_auth_secret
TURN_SERVER="your-turn-server.keeperpamlab.com"  # Get this from: terraform output turn_server_hostname

# Generate credentials
TIMESTAMP=$(($(date +%s) + 3600))  # Valid for 1 hour
USERNAME="${TIMESTAMP}:bash-user"
PASSWORD=$(echo -n "$USERNAME" | openssl dgst -sha1 -hmac "$TURN_SECRET" -binary | base64)

echo "TURN Server: $TURN_SERVER"
echo "Username: $USERNAME"
echo "Password: $PASSWORD"
echo ""
echo "Test with turnutils_uclient:"
echo "turnutils_uclient -t -u \"$USERNAME\" -w \"$PASSWORD\" $TURN_SERVER"
```

**One-liner for quick testing:**
```bash
# Set your values
SECRET="your-secret" && SERVER="your-server.com" && \
USER="$(($(date +%s) + 3600)):test" && \
PASS=$(echo -n "$USER" | openssl dgst -sha1 -hmac "$SECRET" -binary | base64) && \
echo "turnutils_uclient -t -u \"$USER\" -w \"$PASS\" $SERVER"
```

### 4. Security Considerations

- **Keep the secret secure**: The `turn_auth_secret` should be treated as a sensitive credential
- **Rotate regularly**: Consider rotating the secret periodically
- **Use strong secrets**: Use a cryptographically strong random string
- **Time-based credentials**: The generated credentials have an expiration time built in

### 5. Testing the Authentication

You can test the TURN server authentication using the `turnutils_uclient` tool:

```bash
# Install coturn client tools (on your local machine)
# Ubuntu/Debian: sudo apt-get install coturn-utils
# macOS: brew install coturn

# Generate credentials using your secret
USERNAME="$(date +%s):test-user"
PASSWORD=$(echo -n "$USERNAME" | openssl dgst -sha1 -hmac "your-super-secure-secret-here" -binary | base64)

# Test the TURN server
turnutils_uclient -t -u "$USERNAME" -w "$PASSWORD" your-turn-server.keeperpamlab.com
```

This setup provides secure, time-limited access to your TURN server while maintaining the shared secret approach that's commonly used in production WebRTC applications. 
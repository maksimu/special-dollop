#!/bin/bash

# TURN Server Credential Generator and Tester
# Usage: ./test_turn_credentials.sh [OPTIONS]
#   -s, --secret SECRET     Auth secret (auto-detected from terraform if not provided)
#   -h, --host SERVER       Server hostname (auto-detected from terraform if not provided)  
#   -u, --user USERNAME     Username suffix (default: current user)
#   -t, --time HOURS        Expiration hours (default: 48)
#   --test                  Run turnutils_uclient test after generating credentials
#   --help                  Show this help

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Default values
DEFAULT_HOURS=48
DEFAULT_USER=$(whoami)
RUN_TEST=false
TURN_SECRET=""
TURN_SERVER=""
USER_SUFFIX="$DEFAULT_USER"
EXPIRY_HOURS="$DEFAULT_HOURS"

# Help function
show_help() {
    echo "TURN Server Credential Generator and Tester"
    echo ""
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  -s, --secret SECRET    Auth secret (auto-detected from terraform)"
    echo "  -h, --host SERVER      Server hostname (auto-detected from terraform)"
    echo "  -u, --user USERNAME    Username suffix (default: $DEFAULT_USER)"
    echo "  -t, --time HOURS       Expiration hours (default: $DEFAULT_HOURS)"
    echo "  --test                 Run turnutils_uclient test after generating"
    echo "  --help                 Show this help"
    echo ""
    echo "Examples:"
    echo "  $0                                    # 48h creds for $DEFAULT_USER, auto-detect server"
    echo "  $0 -u alice -t 24                    # 24h creds for alice"
    echo "  $0 --test                            # Generate and test"
    echo "  $0 -s my-secret -h turn.example.com  # Manual server/secret"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -s|--secret)
            TURN_SECRET="$2"
            shift 2
            ;;
        -h|--host)
            TURN_SERVER="$2"
            shift 2
            ;;
        -u|--user)
            USER_SUFFIX="$2"
            shift 2
            ;;
        -t|--time)
            EXPIRY_HOURS="$2"
            shift 2
            ;;
        --test)
            RUN_TEST=true
            shift
            ;;
        --help)
            show_help
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            show_help
            exit 1
            ;;
    esac
done

echo -e "${BLUE}TURN Server Credential Generator & Tester${NC}"
echo "============================================="

# Auto-detect values from terraform if not provided
if [ -z "$TURN_SECRET" ] || [ -z "$TURN_SERVER" ]; then
    if command -v terraform &> /dev/null && [ -f "terraform.tfstate" ]; then
        echo -e "${YELLOW}📋 Auto-detecting from Terraform...${NC}"
        
        if [ -z "$TURN_SECRET" ]; then
            TURN_SECRET=$(terraform output -raw turn_server_auth_secret 2>/dev/null || echo "")
        fi
        
        if [ -z "$TURN_SERVER" ]; then
            TURN_SERVER=$(terraform output -raw turn_server_hostname 2>/dev/null || echo "")
        fi
        
        if [ -z "$TURN_SECRET" ] || [ -z "$TURN_SERVER" ]; then
            echo -e "${RED}Could not auto-detect values from Terraform${NC}"
            echo "Please provide them manually with -s and -h options"
            exit 1
        fi
    else
        echo -e "${RED}No terraform state found and no manual values provided${NC}"
        echo "Please provide secret and server manually:"
        echo "  $0 -s \"your-secret\" -h \"turn.example.com\""
        exit 1
    fi
fi

# Calculate expiration timestamp
EXPIRY_SECONDS=$((EXPIRY_HOURS * 3600))
TIMESTAMP=$(($(date +%s) + EXPIRY_SECONDS))
USERNAME="${TIMESTAMP}:${USER_SUFFIX}"

# Generate password using HMAC-SHA1
echo -e "${YELLOW}Generating credentials...${NC}"
PASSWORD=$(echo -n "$USERNAME" | openssl dgst -sha1 -hmac "$TURN_SECRET" -binary | base64)

# Calculate and display expiration
EXPIRES_DATE=$(date -d "@$TIMESTAMP" 2>/dev/null || date -r "$TIMESTAMP" 2>/dev/null || echo "$(date) + ${EXPIRY_HOURS}h")

# Display results
echo ""
echo -e "${GREEN}${EXPIRY_HOURS}-Hour TURN Credentials Generated:${NC}"
echo "================================================"
echo "Server:   $TURN_SERVER"
echo "Username: $USERNAME"
echo "Password: $PASSWORD"
echo "Expires:  $EXPIRES_DATE"
echo ""

# WebRTC configuration example
echo -e "${BLUE}WebRTC ICE Configuration:${NC}"
echo "============================"
echo "{"
echo "  iceServers: ["
echo "    {"
echo "      urls: ["
echo "        \"stun:${TURN_SERVER}:3478\","
echo "        \"turn:${TURN_SERVER}:3478\""
echo "      ],"
echo "      username: \"$USERNAME\","
echo "      credential: \"$PASSWORD\""
echo "    }"
echo "  ]"
echo "}"
echo ""

# Test commands
echo -e "${BLUE}Manual Test Commands:${NC}"
echo "========================"
echo "turnutils_uclient -t -y -u \"$USERNAME\" -w \"$PASSWORD\" $TURN_SERVER"
echo "turnutils_stunclient $TURN_SERVER"
echo ""

# Optional automatic test
if [ "$RUN_TEST" = true ]; then
    if command -v turnutils_uclient &> /dev/null; then
        echo -e "${YELLOW}Running automatic TURN test...${NC}"
        echo "===================================="
        echo ""
        turnutils_uclient -t -y -u "$USERNAME" -w "$PASSWORD" "$TURN_SERVER" || {
            echo -e "${RED}TURN test failed. Check server status and network connectivity.${NC}"
            exit 1
        }
        echo ""
        echo -e "${GREEN}TURN server test completed successfully!${NC}"
    else
        echo -e "${YELLOW}turnutils_uclient not found. Install coturn-utils to run automatic tests:${NC}"
        echo "   Ubuntu/Debian: sudo apt-get install coturn-utils"
        echo "   macOS:         brew install coturn"
        echo "   CentOS/RHEL:   sudo yum install coturn-utils"
    fi
else
    echo -e "${YELLOW}Add --test flag to run automatic testing${NC}"
fi

echo ""
echo -e "${GREEN}Done! Use the credentials above in your WebRTC application.${NC}"
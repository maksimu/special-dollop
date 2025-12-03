#!/usr/bin/env bash
#
# Run all integration tests for guacr
#
# This script:
# 1. Starts all required Docker containers (SSH, Telnet, RDP, VNC)
# 2. Waits for services to be healthy
# 3. Runs all tests (unit, integration, Docker-based)
# 4. Reports results
# 5. Optionally cleans up containers
#
# Usage:
#   ./scripts/run-all-tests.sh              # Run all tests, keep containers
#   ./scripts/run-all-tests.sh --clean      # Run all tests, stop containers after
#   ./scripts/run-all-tests.sh --quick      # Run only unit tests (no Docker)
#   ./scripts/run-all-tests.sh --protocol ssh,telnet  # Run specific protocols only
#

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Parse arguments
CLEANUP=false
QUICK=false
PROTOCOLS="ssh,telnet,rdp,vnc"

while [[ $# -gt 0 ]]; do
    case $1 in
        --clean|-c)
            CLEANUP=true
            shift
            ;;
        --quick|-q)
            QUICK=true
            shift
            ;;
        --protocol|-p)
            PROTOCOLS="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --clean, -c          Stop Docker containers after tests"
            echo "  --quick, -q          Run only unit tests (no Docker)"
            echo "  --protocol, -p LIST  Run specific protocols (comma-separated)"
            echo "                       Options: ssh,telnet,rdp,vnc,database"
            echo ""
            echo "Examples:"
            echo "  $0                      # Run all tests"
            echo "  $0 --quick              # Unit tests only"
            echo "  $0 --protocol ssh,vnc   # SSH and VNC tests only"
            echo "  $0 --clean              # Full tests, cleanup after"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║          guacr Full Integration Test Suite                 ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

cd "$PROJECT_ROOT"

# Track results (using simple arrays for compatibility)
PASSED_TESTS=""
FAILED_TESTS=""
TOTAL_PASSED=0
TOTAL_FAILED=0

# ============================================================================
# Helper Functions
# ============================================================================

run_test() {
    local name="$1"
    local cmd="$2"
    local logfile="/tmp/guacr-test-${name}.log"
    
    echo -e "${CYAN}  Running: $name${NC}"
    
    if eval "$cmd" > "$logfile" 2>&1; then
        PASSED_TESTS="$PASSED_TESTS $name"
        TOTAL_PASSED=$((TOTAL_PASSED + 1))
        echo -e "  ${GREEN}✓ $name passed${NC}"
        return 0
    else
        FAILED_TESTS="$FAILED_TESTS $name"
        TOTAL_FAILED=$((TOTAL_FAILED + 1))
        echo -e "  ${RED}✗ $name failed${NC}"
        echo -e "  ${YELLOW}  See: $logfile${NC}"
        return 1
    fi
}

wait_for_port() {
    local host="$1"
    local port="$2"
    local name="$3"
    local timeout="${4:-30}"
    
    echo -n "  Waiting for $name on port $port..."
    for i in $(seq 1 $timeout); do
        if nc -z "$host" "$port" 2>/dev/null; then
            echo -e " ${GREEN}ready${NC}"
            return 0
        fi
        sleep 1
        echo -n "."
    done
    echo -e " ${RED}timeout${NC}"
    return 1
}

# ============================================================================
# Step 1: Unit Tests (Always Run)
# ============================================================================

echo -e "${YELLOW}[Step 1] Running Unit Tests${NC}"
echo ""

# Core library tests
run_test "guacr-protocol" "cargo test -p guacr-protocol" || true
run_test "guacr-terminal" "cargo test -p guacr-terminal" || true
run_test "guacr-handlers-unit" "cargo test -p guacr-handlers --lib" || true

# Handler unit tests
run_test "guacr-ssh-unit" "cargo test -p guacr-ssh --lib" || true
run_test "guacr-telnet-unit" "cargo test -p guacr-telnet --lib" || true
run_test "guacr-rdp-unit" "cargo test -p guacr-rdp --lib" || true
run_test "guacr-vnc-unit" "cargo test -p guacr-vnc --lib" || true

# Pipe stream tests (no Docker needed)
run_test "pipe-integration" "cargo test -p guacr-handlers --test pipe_integration_test" || true

if [ "$QUICK" = true ]; then
    echo ""
    echo -e "${YELLOW}Quick mode - skipping Docker-based tests${NC}"
    echo ""
else
    # ============================================================================
    # Step 2: Start Docker Containers
    # ============================================================================

    echo ""
    echo -e "${YELLOW}[Step 2] Starting Docker Containers${NC}"
    echo ""

    # Start containers based on selected protocols
    DOCKER_SERVICES=""
    
    if [[ "$PROTOCOLS" == *"ssh"* ]]; then
        DOCKER_SERVICES="$DOCKER_SERVICES ssh"
    fi
    if [[ "$PROTOCOLS" == *"telnet"* ]]; then
        DOCKER_SERVICES="$DOCKER_SERVICES telnet"
    fi
    if [[ "$PROTOCOLS" == *"rdp"* ]]; then
        DOCKER_SERVICES="$DOCKER_SERVICES rdp"
    fi
    if [[ "$PROTOCOLS" == *"vnc"* ]]; then
        DOCKER_SERVICES="$DOCKER_SERVICES vnc-light"  # Use lighter VNC
    fi

    if [ -n "$DOCKER_SERVICES" ]; then
        echo "  Starting: $DOCKER_SERVICES"
        docker-compose -f docker-compose.test.yml up -d $DOCKER_SERVICES 2>&1 | grep -v "obsolete" || true
    fi

    # ============================================================================
    # Step 3: Wait for Services
    # ============================================================================

    echo ""
    echo -e "${YELLOW}[Step 3] Waiting for Services${NC}"
    echo ""

    if [[ "$PROTOCOLS" == *"ssh"* ]]; then
        wait_for_port localhost 2222 "SSH" 30 || true
    fi
    if [[ "$PROTOCOLS" == *"telnet"* ]]; then
        wait_for_port localhost 2323 "Telnet" 30 || true
    fi
    if [[ "$PROTOCOLS" == *"rdp"* ]]; then
        wait_for_port localhost 3389 "RDP" 60 || true  # RDP takes longer
    fi
    if [[ "$PROTOCOLS" == *"vnc"* ]]; then
        wait_for_port localhost 5901 "VNC" 30 || true
    fi

    # Extra wait for services to fully initialize
    sleep 3

    # ============================================================================
    # Step 4: Docker-based Integration Tests
    # ============================================================================

    echo ""
    echo -e "${YELLOW}[Step 4] Running Docker-based Integration Tests${NC}"
    echo ""

    if [[ "$PROTOCOLS" == *"ssh"* ]]; then
        run_test "ssh-integration" "cargo test -p guacr-ssh --test integration_test -- --include-ignored" || true
    fi

    if [[ "$PROTOCOLS" == *"telnet"* ]]; then
        run_test "telnet-integration" "cargo test -p guacr-telnet --test integration_test -- --include-ignored" || true
    fi

    if [[ "$PROTOCOLS" == *"rdp"* ]]; then
        run_test "rdp-integration" "cargo test -p guacr-rdp --test integration_test -- --include-ignored" || true
    fi

    if [[ "$PROTOCOLS" == *"vnc"* ]]; then
        run_test "vnc-integration" "cargo test -p guacr-vnc --test integration_test -- --include-ignored" || true
    fi

    # ============================================================================
    # Step 5: Database Tests (if requested)
    # ============================================================================

    if [[ "$PROTOCOLS" == *"database"* ]]; then
        echo ""
        echo -e "${YELLOW}[Step 5] Starting Database Containers${NC}"
        echo ""
        
        cd crates/guacr-database
        docker-compose -f docker-compose.test.yml up -d 2>&1 | grep -v "obsolete" || true
        cd "$PROJECT_ROOT"
        
        # Wait for databases
        wait_for_port localhost 13306 "MySQL" 30 || true
        wait_for_port localhost 15432 "PostgreSQL" 30 || true
        wait_for_port localhost 16379 "Redis" 30 || true
        wait_for_port localhost 17017 "MongoDB" 30 || true
        
        sleep 5
        
        run_test "database-integration" "cargo test -p guacr-database --test integration_test" || true
    fi
fi

# ============================================================================
# Cleanup (if requested)
# ============================================================================

if [ "$CLEANUP" = true ]; then
    echo ""
    echo -e "${YELLOW}Cleaning up Docker containers...${NC}"
    docker-compose -f docker-compose.test.yml down 2>&1 | grep -v "obsolete" || true
    
    if [[ "$PROTOCOLS" == *"database"* ]]; then
        cd crates/guacr-database
        docker-compose -f docker-compose.test.yml down 2>&1 | grep -v "obsolete" || true
        cd "$PROJECT_ROOT"
    fi
    
    echo -e "${GREEN}  ✓ Containers stopped${NC}"
fi

# ============================================================================
# Summary
# ============================================================================

echo ""
echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║                    Test Results Summary                     ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
echo ""

# Show passed tests
for test_name in $PASSED_TESTS; do
    echo -e "  ${GREEN}✓${NC} $test_name"
done

# Show failed tests
for test_name in $FAILED_TESTS; do
    echo -e "  ${RED}✗${NC} $test_name"
done

echo ""
echo -e "  ${GREEN}Passed: $TOTAL_PASSED${NC}  ${RED}Failed: $TOTAL_FAILED${NC}"
echo ""

if [ $TOTAL_FAILED -eq 0 ]; then
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║                  All tests passed! ✓                       ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    exit 0
else
    echo -e "${RED}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║              $TOTAL_FAILED test suite(s) failed                        ║${NC}"
    echo -e "${RED}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo "Check log files in /tmp/guacr-test-*.log for details"
    exit 1
fi

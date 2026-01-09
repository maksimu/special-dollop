#!/usr/bin/env bash
#
# Integration Test Runner with Enhanced Output
#
# This script provides a better developer experience for running integration tests:
# - Automatic Docker container management
# - Real-time progress indicators
# - Detailed error reporting
# - Test result summaries
# - Performance metrics
#
# Usage:
#   ./scripts/test-integration.sh [PROTOCOL]
#
# Examples:
#   ./scripts/test-integration.sh           # Run all integration tests
#   ./scripts/test-integration.sh ssh       # Run SSH tests only
#   ./scripts/test-integration.sh rdp vnc   # Run RDP and VNC tests

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
BOLD='\033[1m'
NC='\033[0m'

# Configuration
DOCKER_COMPOSE_FILE="$PROJECT_ROOT/docker-compose.test.yml"
DB_DOCKER_COMPOSE_FILE="$PROJECT_ROOT/crates/guacr-database/docker-compose.test.yml"

# Test results
declare -A TEST_RESULTS
declare -A TEST_DURATIONS
TOTAL_TESTS=0
PASSED_TESTS=0
FAILED_TESTS=0

# ============================================================================
# Helper Functions
# ============================================================================

print_header() {
    echo ""
    echo -e "${BLUE}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║${NC}  $1"
    echo -e "${BLUE}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

print_section() {
    echo ""
    echo -e "${CYAN}▶ $1${NC}"
    echo ""
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_info() {
    echo -e "${BLUE}ℹ${NC} $1"
}

spinner() {
    local pid=$1
    local message=$2
    local spinstr='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'
    
    while kill -0 $pid 2>/dev/null; do
        local temp=${spinstr#?}
        printf " ${CYAN}%c${NC} %s\r" "$spinstr" "$message"
        spinstr=$temp${spinstr%"$temp"}
        sleep 0.1
    done
    printf "    \r"
}

check_port() {
    local host=$1
    local port=$2
    timeout 1 bash -c "cat < /dev/null > /dev/tcp/$host/$port" 2>/dev/null
}

wait_for_port() {
    local host=$1
    local port=$2
    local name=$3
    local timeout=${4:-30}
    local start_time=$(date +%s)
    
    echo -n "  Waiting for $name on port $port"
    
    for i in $(seq 1 $timeout); do
        if check_port "$host" "$port"; then
            local end_time=$(date +%s)
            local duration=$((end_time - start_time))
            echo -e " ${GREEN}✓${NC} (${duration}s)"
            return 0
        fi
        echo -n "."
        sleep 1
    done
    
    echo -e " ${RED}✗ timeout${NC}"
    return 1
}

run_test() {
    local name=$1
    local crate=$2
    local start_time=$(date +%s)
    
    TOTAL_TESTS=$((TOTAL_TESTS + 1))
    
    echo -e "${CYAN}▶${NC} Testing: ${BOLD}$name${NC}"
    
    local log_file="/tmp/guacr-test-$name-$$.log"
    
    if cargo test -p "$crate" --test integration_test -- --include-ignored --nocapture > "$log_file" 2>&1; then
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        
        TEST_RESULTS[$name]="PASS"
        TEST_DURATIONS[$name]=$duration
        PASSED_TESTS=$((PASSED_TESTS + 1))
        
        # Extract test count
        local test_count=$(grep -oE "[0-9]+ passed" "$log_file" | head -1 | grep -oE "[0-9]+" || echo "?")
        
        print_success "$name (${test_count} tests, ${duration}s)"
        rm -f "$log_file"
        return 0
    else
        local end_time=$(date +%s)
        local duration=$((end_time - start_time))
        
        TEST_RESULTS[$name]="FAIL"
        TEST_DURATIONS[$name]=$duration
        FAILED_TESTS=$((FAILED_TESTS + 1))
        
        print_error "$name (${duration}s)"
        print_warning "Log saved to: $log_file"
        
        # Show last 10 lines of error
        echo ""
        echo -e "${YELLOW}Last 10 lines of output:${NC}"
        tail -10 "$log_file" | sed 's/^/  /'
        echo ""
        
        return 1
    fi
}

start_docker_service() {
    local service=$1
    local compose_file=${2:-$DOCKER_COMPOSE_FILE}
    
    echo -n "  Starting $service..."
    if docker-compose -f "$compose_file" up -d "$service" > /dev/null 2>&1; then
        echo -e " ${GREEN}✓${NC}"
        return 0
    else
        echo -e " ${RED}✗${NC}"
        return 1
    fi
}

check_docker() {
    if ! command -v docker &> /dev/null; then
        print_error "Docker is not installed or not in PATH"
        exit 1
    fi
    
    if ! docker info &> /dev/null; then
        print_error "Docker daemon is not running"
        exit 1
    fi
}

# ============================================================================
# Protocol Test Functions
# ============================================================================

test_ssh() {
    print_section "SSH Integration Tests"
    
    start_docker_service "ssh"
    wait_for_port "localhost" "2222" "SSH" 30 || {
        print_error "SSH server failed to start"
        return 1
    }
    
    run_test "SSH" "guacr-ssh"
}

test_rdp() {
    print_section "RDP Integration Tests"
    
    start_docker_service "rdp"
    wait_for_port "localhost" "3389" "RDP" 60 || {
        print_error "RDP server failed to start"
        return 1
    }
    
    # RDP needs extra time to fully initialize
    sleep 5
    
    run_test "RDP" "guacr-rdp"
}

test_vnc() {
    print_section "VNC Integration Tests"
    
    start_docker_service "vnc-light"
    wait_for_port "localhost" "5901" "VNC" 30 || {
        print_error "VNC server failed to start"
        return 1
    }
    
    run_test "VNC" "guacr-vnc"
}

test_telnet() {
    print_section "Telnet Integration Tests"
    
    start_docker_service "telnet"
    wait_for_port "localhost" "2323" "Telnet" 30 || {
        print_error "Telnet server failed to start"
        return 1
    }
    
    run_test "Telnet" "guacr-telnet"
}

test_sftp() {
    print_section "SFTP Integration Tests"
    
    # SFTP uses the SSH server
    start_docker_service "ssh"
    wait_for_port "localhost" "2222" "SSH/SFTP" 30 || {
        print_error "SSH server failed to start"
        return 1
    }
    
    run_test "SFTP" "guacr-sftp"
}

test_database() {
    print_section "Database Integration Tests"
    
    print_info "Starting database servers (this may take a while)..."
    
    # Start all database services
    cd "$PROJECT_ROOT/crates/guacr-database"
    docker-compose -f docker-compose.test.yml up -d > /dev/null 2>&1
    cd "$PROJECT_ROOT"
    
    # Wait for each database
    wait_for_port "localhost" "13306" "MySQL" 30 || print_warning "MySQL not ready"
    wait_for_port "localhost" "15432" "PostgreSQL" 30 || print_warning "PostgreSQL not ready"
    wait_for_port "localhost" "16379" "Redis" 30 || print_warning "Redis not ready"
    wait_for_port "localhost" "17017" "MongoDB" 30 || print_warning "MongoDB not ready"
    
    # Extra time for databases to initialize
    sleep 5
    
    run_test "Database" "guacr-database"
}

# ============================================================================
# Main Script
# ============================================================================

main() {
    cd "$PROJECT_ROOT"
    
    print_header "Integration Test Runner"
    
    # Check prerequisites
    check_docker
    
    # Parse arguments
    local protocols=("$@")
    if [ ${#protocols[@]} -eq 0 ]; then
        protocols=("ssh" "telnet" "vnc" "rdp" "sftp" "database")
    fi
    
    print_info "Running tests for: ${protocols[*]}"
    print_info "Docker Compose: $DOCKER_COMPOSE_FILE"
    echo ""
    
    # Run requested tests
    for protocol in "${protocols[@]}"; do
        case "$protocol" in
            ssh)
                test_ssh || true
                ;;
            rdp)
                test_rdp || true
                ;;
            vnc)
                test_vnc || true
                ;;
            telnet)
                test_telnet || true
                ;;
            sftp)
                test_sftp || true
                ;;
            database|db)
                test_database || true
                ;;
            *)
                print_error "Unknown protocol: $protocol"
                ;;
        esac
    done
    
    # Print summary
    print_header "Test Results Summary"
    
    echo -e "${BOLD}Results by Protocol:${NC}"
    echo ""
    
    for name in "${!TEST_RESULTS[@]}"; do
        local result="${TEST_RESULTS[$name]}"
        local duration="${TEST_DURATIONS[$name]}"
        
        if [ "$result" = "PASS" ]; then
            printf "  ${GREEN}✓${NC} %-20s ${GREEN}PASSED${NC}  (%ds)\n" "$name" "$duration"
        else
            printf "  ${RED}✗${NC} %-20s ${RED}FAILED${NC}  (%ds)\n" "$name" "$duration"
        fi
    done
    
    echo ""
    echo -e "${BOLD}Summary:${NC}"
    echo -e "  Total:  $TOTAL_TESTS"
    echo -e "  ${GREEN}Passed: $PASSED_TESTS${NC}"
    echo -e "  ${RED}Failed: $FAILED_TESTS${NC}"
    echo ""
    
    # Calculate total duration
    local total_duration=0
    for duration in "${TEST_DURATIONS[@]}"; do
        total_duration=$((total_duration + duration))
    done
    
    echo -e "${BOLD}Total Time:${NC} ${total_duration}s"
    echo ""
    
    # Cleanup suggestion
    print_info "To stop test servers: make docker-down"
    print_info "To view logs: make docker-logs"
    echo ""
    
    # Exit with appropriate code
    if [ $FAILED_TESTS -eq 0 ]; then
        print_header "All Tests Passed! ✓"
        exit 0
    else
        print_header "Some Tests Failed"
        exit 1
    fi
}

# Run main function
main "$@"

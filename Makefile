# Makefile for guacr development and testing
#
# Quick reference:
#   make test          - Run all unit tests
#   make test-quick    - Run unit tests only (no Docker)
#   make test-full     - Run all tests including Docker integration
#   make docker-up     - Start all test servers
#   make docker-down   - Stop all test servers
#   make docker-logs   - View logs from test servers
#   make clean         - Clean build artifacts and stop containers
#   make fmt           - Format code
#   make clippy        - Run clippy lints
#   make ci            - Run full CI checks (clippy + fmt + test)

.PHONY: help test test-quick test-full docker-up docker-down docker-logs clean fmt clippy ci

# Default target
help:
	@echo "guacr Development Commands"
	@echo ""
	@echo "Testing:"
	@echo "  make test                - Run all unit tests"
	@echo "  make test-quick          - Run unit tests only (no Docker)"
	@echo "  make test-full           - Run all tests including Docker integration"
	@echo "  make test-terminal       - Run terminal emulator tests"
	@echo "  make test-terminal-modes - Run application cursor mode tests"
	@echo "  make test-terminal-mouse - Run mouse mode and text selection tests"
	@echo "  make test-ssh            - Run SSH integration tests"
	@echo "  make test-rdp           - Run RDP integration tests"
	@echo "  make test-vnc           - Run VNC integration tests"
	@echo "  make test-telnet        - Run Telnet integration tests"
	@echo "  make test-db            - Run database integration tests"
	@echo ""
	@echo "Docker Management:"
	@echo "  make docker-up     - Start all test servers"
	@echo "  make docker-down   - Stop all test servers"
	@echo "  make docker-logs   - View logs from test servers"
	@echo "  make docker-clean  - Remove all test containers and volumes"
	@echo ""
	@echo "Code Quality:"
	@echo "  make fmt           - Format code with cargo fmt"
	@echo "  make clippy        - Run clippy lints"
	@echo "  make clippy-fix    - Auto-fix clippy warnings"
	@echo "  make ci            - Run full CI checks (clippy + fmt + test)"
	@echo ""
	@echo "Build:"
	@echo "  make build         - Build all crates"
	@echo "  make build-release - Build release version"
	@echo "  make clean         - Clean build artifacts"

# ============================================================================
# Testing
# ============================================================================

test:
	@echo "Running all unit tests..."
	cargo test --workspace --lib

test-quick:
	@echo "Running quick unit tests (no Docker)..."
	./scripts/run-all-tests.sh --quick

test-full:
	@echo "Running full integration tests (with Docker)..."
	./scripts/run-all-tests.sh

test-terminal:
	@echo "Running terminal emulator tests..."
	@echo ""
	@echo "Testing terminal emulation and key handling..."
	@cargo test -p guacr-terminal
	@echo ""
	@echo "✓ All terminal tests passed"

test-terminal-modes:
	@echo "Running application cursor mode tests..."
	@cargo test -p guacr-terminal application_cursor
	@cargo test -p guacr-terminal mode_switching
	@echo ""
	@echo "✓ Mode switching tests passed"

test-terminal-mouse:
	@echo "Running mouse mode and text selection tests..."
	@cargo test -p guacr-terminal mouse_mode
	@cargo test -p guacr-terminal text_selection
	@echo ""
	@echo "✓ Mouse mode tests passed"

test-ssh: docker-up-ssh
	@echo "Running SSH integration tests..."
	cargo test -p guacr-ssh --test integration_test -- --include-ignored

test-rdp: docker-up-rdp
	@echo "Running RDP integration tests..."
	cargo test -p guacr-rdp --test integration_test -- --include-ignored

test-vnc: docker-up-vnc
	@echo "Running VNC integration tests..."
	cargo test -p guacr-vnc --test integration_test -- --include-ignored

test-telnet: docker-up-telnet
	@echo "Running Telnet integration tests..."
	cargo test -p guacr-telnet --test integration_test -- --include-ignored

test-db: docker-up-db
	@echo "Running database integration tests..."
	cargo test -p guacr-database --test integration_test

test-sftp: docker-up-ssh
	@echo "Running SFTP integration tests..."
	cargo test -p guacr-sftp --test integration_test -- --include-ignored

test-rbi: 
	@echo "Running RBI integration tests..."
	cargo test -p guacr-rbi --test integration_test --features chrome -- --include-ignored

test-threat-detection:
	@echo "Running threat detection tests..."
	cargo test -p guacr-threat-detection --test integration_test -- --include-ignored

test-performance:
	@echo "Running performance tests..."
	cargo test -p guacr-handlers --test performance_test -- --include-ignored

test-security:
	@echo "Running security tests..."
	cargo test -p guacr-handlers --test security_test -- --include-ignored

# ============================================================================
# Docker Management
# ============================================================================

docker-up:
	@echo "Starting all test servers..."
	docker-compose -f docker-compose.test.yml up -d
	@echo "Waiting for services to be ready..."
	@sleep 5
	@echo "Services started. Run 'make docker-logs' to view logs."

docker-up-ssh:
	@echo "Starting SSH test server..."
	docker-compose -f docker-compose.test.yml up -d ssh
	@sleep 2

docker-up-rdp:
	@echo "Starting RDP test server..."
	docker-compose -f docker-compose.test.yml up -d rdp
	@sleep 10

docker-up-vnc:
	@echo "Starting VNC test server..."
	docker-compose -f docker-compose.test.yml up -d vnc-light
	@sleep 5

docker-up-telnet:
	@echo "Starting Telnet test server..."
	docker-compose -f docker-compose.test.yml up -d telnet
	@sleep 2

docker-up-db:
	@echo "Starting database test servers..."
	cd crates/guacr-database && docker-compose -f docker-compose.test.yml up -d
	@sleep 10

docker-down:
	@echo "Stopping all test servers..."
	docker-compose -f docker-compose.test.yml down
	cd crates/guacr-database && docker-compose -f docker-compose.test.yml down

docker-logs:
	docker-compose -f docker-compose.test.yml logs -f

docker-clean:
	@echo "Removing all test containers and volumes..."
	docker-compose -f docker-compose.test.yml down -v
	cd crates/guacr-database && docker-compose -f docker-compose.test.yml down -v

docker-status:
	@echo "Test server status:"
	@docker-compose -f docker-compose.test.yml ps

# ============================================================================
# Code Quality
# ============================================================================

fmt:
	@echo "Formatting code..."
	cargo fmt --all

fmt-check:
	@echo "Checking code formatting..."
	cargo fmt --all -- --check

clippy:
	@echo "Running clippy..."
	cargo clippy --all-targets -- -D warnings

clippy-fix:
	@echo "Auto-fixing clippy warnings..."
	cargo clippy --fix --workspace --allow-dirty --allow-staged

ci: fmt-check clippy test
	@echo ""
	@echo "✅ All CI checks passed!"

# ============================================================================
# Build
# ============================================================================

build:
	@echo "Building all crates..."
	cargo build --workspace

build-release:
	@echo "Building release version..."
	cargo build --workspace --release

clean:
	@echo "Cleaning build artifacts..."
	cargo clean
	@echo "Stopping test containers..."
	-@make docker-down 2>/dev/null

# ============================================================================
# Development Helpers
# ============================================================================

watch:
	@echo "Watching for changes and running tests..."
	cargo watch -x "test --workspace --lib"

doc:
	@echo "Building documentation..."
	cargo doc --workspace --no-deps --open

check:
	@echo "Running cargo check..."
	cargo check --workspace

# ============================================================================
# Protocol-Specific Development
# ============================================================================

dev-ssh: docker-up-ssh
	@echo "SSH server running on localhost:2222"
	@echo "  Username: test_user"
	@echo "  Password: test_password"
	@echo ""
	@echo "Test with: ssh -p 2222 test_user@localhost"
	@echo "Or run: cargo test -p guacr-ssh --test integration_test -- --include-ignored"

dev-rdp: docker-up-rdp
	@echo "RDP server running on localhost:3389"
	@echo "  Username: test_user"
	@echo "  Password: test_password"
	@echo ""
	@echo "Test with: xfreerdp /v:localhost:3389 /u:test_user /p:test_password"
	@echo "Or run: cargo test -p guacr-rdp --test integration_test -- --include-ignored"

dev-vnc: docker-up-vnc
	@echo "VNC server running on localhost:5901"
	@echo "  Password: test_password"
	@echo ""
	@echo "Test with: vncviewer localhost:5901"
	@echo "Or run: cargo test -p guacr-vnc --test integration_test -- --include-ignored"

dev-db: docker-up-db
	@echo "Database servers running:"
	@echo "  MySQL:      localhost:13306 (testuser/testpassword)"
	@echo "  PostgreSQL: localhost:15432 (testuser/testpassword)"
	@echo "  MongoDB:    localhost:17017 (testuser/testpassword)"
	@echo "  Redis:      localhost:16379 (password: testpassword)"
	@echo "  SQL Server: localhost:11433 (sa/TestPassword123!)"
	@echo ""
	@echo "Test with: cargo test -p guacr-database --test integration_test"

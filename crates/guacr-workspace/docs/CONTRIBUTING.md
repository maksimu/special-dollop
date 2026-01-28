# Contributing to Guacr

Thank you for your interest in contributing to Guacr! This document provides guidelines for contributing to the project.

## Code of Conduct

- Be respectful and professional
- Focus on technical merit
- Help create a welcoming environment for all contributors

## Development Guidelines

### Code Style

1. **Follow Rust Conventions**
   - Run `cargo fmt` before committing
   - Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
   - Use meaningful variable and function names

2. **No Emojis in Code or Documentation**
   - Do not use emojis in any source code
   - Do not use emojis in documentation (markdown, comments, etc.)
   - Use clear, professional language instead

3. **Documentation**
   - Document all public APIs
   - Include usage examples
   - Keep documentation up-to-date with code changes

4. **Error Handling**
   - Use `Result` and `Option` appropriately
   - Provide meaningful error messages
   - Use `thiserror` or `anyhow` for error types

### Testing

1. **All New Code Must Have Tests**
   - Unit tests for individual functions
   - Integration tests for protocol handlers
   - Performance benchmarks for critical paths

2. **Test Coverage**
   - Aim for >80% code coverage
   - Test error paths, not just happy paths
   - Include edge cases

3. **Running Tests**
   ```bash
   # All tests
   cargo test

   # Specific crate
   cargo test -p guacr-protocol

   # With output
   cargo test -- --nocapture
   ```

### Performance

1. **Benchmark Critical Code**
   - Use `cargo bench` for performance-critical code
   - Document performance characteristics
   - Profile before optimizing

2. **Memory Efficiency**
   - Use buffer pools where appropriate
   - Avoid unnecessary allocations
   - Use zero-copy techniques when possible

3. **Concurrency**
   - Prefer lock-free data structures
   - Use `async`/`await` for I/O
   - Document thread-safety guarantees

### Security

1. **Security Review**
   - Run `cargo audit` before submitting
   - Review all `unsafe` code carefully
   - Document safety invariants

2. **Input Validation**
   - Validate all external input
   - Use safe parsing libraries
   - Avoid buffer overflows

3. **Cryptography**
   - Use well-tested crypto libraries
   - Follow cryptographic best practices
   - Document security assumptions

## Commit Guidelines

### Commit Messages

Follow conventional commits format:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

**Example:**
```
feat(ssh): add support for Ed25519 keys

Implement Ed25519 key support using russh-keys crate.
Includes tests for key parsing and authentication.

Closes #123
```

### Branch Naming

- `feature/description` - New features
- `fix/description` - Bug fixes
- `docs/description` - Documentation updates
- `perf/description` - Performance improvements

## Pull Request Process

### Before Submitting

1. **Update Your Branch**
   ```bash
   git fetch origin
   git rebase origin/main
   ```

2. **Run All Checks**
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   cargo audit
   ```

3. **Update Documentation**
   - Update README if adding features
   - Update relevant docs in `docs/`
   - Add docstrings to new public APIs

### PR Description

Include in your PR description:

1. **Summary**: What does this PR do?
2. **Motivation**: Why is this change needed?
3. **Implementation**: Key technical decisions
4. **Testing**: How was this tested?
5. **Breaking Changes**: Any API changes?

**Example:**
```markdown
## Summary
Implements zero-copy buffer pooling for SSH handler

## Motivation
Reduces allocations by 95% in SSH connections, improving performance
and reducing latency.

## Implementation
- Added `BufferPool` with crossbeam `ArrayQueue`
- Modified `SshHandler` to use pooled buffers
- Added benchmarks showing 2x throughput improvement

## Testing
- Unit tests for BufferPool
- Integration tests with 1000 concurrent SSH connections
- Benchmarks showing performance improvement

## Breaking Changes
None - internal implementation only
```

### Review Process

1. **Automated Checks**
   - All CI checks must pass
   - Code coverage must not decrease
   - Performance benchmarks must not regress

2. **Code Review**
   - At least one maintainer approval required
   - Address all review comments
   - Keep discussion professional and technical

3. **Final Steps**
   - Squash commits if requested
   - Update PR based on feedback
   - Maintainer will merge when approved

## Development Workflow

### Setting Up Development Environment

1. **Clone Repository**
   ```bash
   git clone https://github.com/keeper/guacr.git
   cd guacr
   ```

2. **Install Dependencies**
   ```bash
   # macOS
   brew install pkg-config openssl protobuf

   # Ubuntu/Debian
   sudo apt-get install build-essential pkg-config libssl-dev protobuf-compiler
   ```

3. **Build Project**
   ```bash
   cargo build
   ```

4. **Run Tests**
   ```bash
   cargo test
   ```

### Development Tools

**Recommended Tools:**
- **cargo-watch**: Auto-rebuild on changes
  ```bash
  cargo install cargo-watch
  cargo watch -x 'run --bin guacr-daemon'
  ```

- **cargo-expand**: Inspect macro expansions
  ```bash
  cargo install cargo-expand
  cargo expand
  ```

- **cargo-audit**: Security auditing
  ```bash
  cargo install cargo-audit
  cargo audit
  ```

- **cargo-flamegraph**: Performance profiling
  ```bash
  cargo install flamegraph
  cargo flamegraph --bin guacr-daemon
  ```

### Debugging

**Enable Debug Logging:**
```bash
RUST_LOG=debug cargo run --bin guacr-daemon
```

**Use rust-gdb or rust-lldb:**
```bash
rust-gdb target/debug/guacr-daemon
```

**Memory Profiling:**
```bash
heaptrack target/release/guacr-daemon
```

## Documentation

### Writing Documentation

1. **Code Documentation**
   - Use `///` for public API documentation
   - Include examples in docstrings
   - Document panics, errors, and safety

   ```rust
   /// Establishes an SSH connection to the remote host.
   ///
   /// # Arguments
   ///
   /// * `params` - Connection parameters from Guacamole handshake
   /// * `guac_tx` - Channel to send Guacamole instructions
   /// * `guac_rx` - Channel to receive Guacamole instructions
   ///
   /// # Errors
   ///
   /// Returns an error if connection fails or authentication fails.
   ///
   /// # Example
   ///
   /// ```no_run
   /// let handler = SshHandler::new(config);
   /// handler.connect(params, tx, rx).await?;
   /// ```
   pub async fn connect(/* ... */) -> Result<()> {
       // ...
   }
   ```

2. **Markdown Documentation**
   - Keep lines under 100 characters
   - Use code blocks with language identifiers
   - Include working examples
   - No emojis

3. **Architecture Decisions**
   - Document in `docs/decisions/`
   - Follow ADR (Architecture Decision Record) format
   - Explain rationale and alternatives considered

## Issue Reporting

### Bug Reports

Include:
1. **Description**: Clear description of the bug
2. **Steps to Reproduce**: Exact steps to trigger the bug
3. **Expected Behavior**: What should happen
4. **Actual Behavior**: What actually happens
5. **Environment**: OS, Rust version, guacr version
6. **Logs**: Relevant log output (use RUST_LOG=debug)

### Feature Requests

Include:
1. **Use Case**: Why is this feature needed?
2. **Proposed Solution**: How should it work?
3. **Alternatives**: Other approaches considered
4. **Additional Context**: Examples, screenshots, etc.

## Communication

### Channels

- **GitHub Issues**: Bug reports and feature requests
- **GitHub Discussions**: General questions and discussions
- **Pull Requests**: Code review and technical discussion

### Response Times

- **Critical Bugs**: Within 24 hours
- **Bug Reports**: Within 3 days
- **Feature Requests**: Within 1 week
- **Pull Requests**: Initial review within 3 days

## License

By contributing to Guacr, you agree that your contributions will be licensed under the same license as the project (MIT OR Apache-2.0).

## Questions?

If you have questions about contributing, please:
1. Check existing documentation
2. Search GitHub Issues
3. Ask in GitHub Discussions
4. Contact maintainers

---

**Thank you for contributing to Guacr!**

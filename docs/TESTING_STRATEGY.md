# Testing Strategy for WebRTC Connection Management

## Overview

This document outlines our comprehensive testing strategy that separates CI-friendly automated tests from resource-intensive manual stress tests.

## 🎯 **Testing Philosophy**

### **CI-First Approach**
- **Deterministic tests**: Predictable behavior across different environments
- **Fast execution**: Complete test suite runs in minutes, not hours
- **Resource-aware**: Designed for shared CI runners with limited resources
- **Fail-fast**: Clear error messages that help developers debug issues quickly

### **Manual Development Testing**
- **Stress testing**: High-load scenarios that may affect system stability
- **Performance benchmarking**: Measuring actual throughput under load
- **Resource exhaustion**: Testing behavior when system limits are reached
- **Environment-specific**: Adapts to available system resources

## 📁 **Test Categories**

### **1. Rust Unit Tests** (`src/tests/*.rs`)
- **Purpose**: Core functionality verification
- **Characteristics**: Fast, deterministic, comprehensive
- **CI Status**: ✅ **Always run in CI**
- **Examples**:
  - Keepalive start/stop mechanisms
  - ICE restart generation
  - Activity tracking accuracy
  - Resource limit enforcement

### **2. Python CI Tests** (`tests/test_*.py`)
- **Purpose**: Integration and business logic testing  
- **Characteristics**: Fast, deterministic, CI-safe
- **CI Status**: ✅ **Always run in CI**
- **Examples**:
  - ICE restart policy logic
  - Connection manager lifecycle
  - Network change handling
  - State transitions

### **3. Manual Stress Tests** (`tests/manual_stress_tests.py`)
- **Purpose**: High-load resource cleanup verification
- **Characteristics**: Resource-intensive, non-deterministic, environment-dependent
- **CI Status**: ❌ **NEVER run in CI** - Manual development only
- **Examples**:
  - Concurrent registry operations under load
  - Memory leak detection with thousands of connections
  - Resource cleanup with intentional race conditions
  - Performance benchmarking under stress

### **4. Performance Benchmarks** (`docs/HOT_PATH_OPTIMIZATION_SUMMARY.md`)
- **Purpose**: Frame processing performance verification
- **Characteristics**: Detailed microsecond-level measurements
- **CI Status**: ⚠️ **Optional** - Can be run in CI for regression detection
- **Examples**:
  - SIMD frame parsing performance
  - UTF-8 character counting benchmarks
  - Memory allocation efficiency
  - Throughput measurements

## 🚫 **What We DON'T Put in CI**

### **Stress Tests with High Error Tolerance**
```python
# ❌ BAD - Too high error rate for production
ACCEPTABLE_ERROR_RATE_THRESHOLD = 0.1  # 10% 

# ✅ GOOD - Strict quality standards
ACCEPTABLE_ERROR_RATE_THRESHOLD = 0.02  # 2%
```

### **Environment-Dependent Timing**
```python
# ❌ BAD - Hardcoded timing assumptions
if total_time < 10.0:  # Will fail on slow CI runners
    pass

# ✅ GOOD - Adaptive to system resources
fast_threshold = 15.0 if is_constrained_system() else 10.0
```

### **Non-Deterministic Race Conditions**
```python
# ❌ BAD - Intentionally creating race conditions
time.sleep(0.01)  # Small delay to increase race condition potential

# ✅ GOOD - Deterministic concurrent testing
async with asyncio.TaskGroup() as group:  # Structured concurrency
    group.create_task(operation1())
    group.create_task(operation2())
```

### **System Resource Exhaustion**
```python
# ❌ BAD - May impact CI system stability
for i in range(10000):  # Create massive number of connections
    create_connection()

# ✅ GOOD - Reasonable resource usage
max_connections = min(100, available_resources() // 2)
```

## 📊 **Quality Metrics**

### **CI Test Quality Standards**
- **Error Rate**: ≤ 1% (much stricter than manual tests)
- **Execution Time**: ≤ 5 minutes for full suite
- **Memory Usage**: ≤ 1GB peak memory
- **Determinism**: 100% consistent results across runs
- **Coverage**: Core functionality completely covered

### **Manual Test Quality Standards**  
- **Error Rate**: ≤ 5% (higher tolerance for stress conditions)
- **Execution Time**: No limit (may run for hours)
- **Memory Usage**: May use significant system resources
- **Determinism**: Results may vary based on system load
- **Coverage**: Edge cases and performance limits

## 🔄 **Test Execution Strategy**

### **Continuous Integration Pipeline**
```bash
# Phase 1: Quick validation (< 1 minute)
cargo check
cargo clippy -- -D warnings

# Phase 2: Unit testing (< 2 minutes)  
cargo test --no-default-features
python3 -m pytest tests/test_*.py -v

# Phase 3: Integration testing (< 2 minutes)
python3 manual_test.py  # Light integration scenarios
python3 network_simulator.py --quick
```

### **Developer Workflow**
```bash
# Daily development testing
cargo test && python3 -m pytest tests/

# Before major commits - include manual stress tests
python3 manual_stress_tests.py --light

# Performance validation - full benchmarks
cargo test test_realistic_frame_processing_performance -- --nocapture
```

### **Release Validation**
```bash
# Comprehensive testing before release
python3 manual_stress_tests.py --full
python3 manual_stress_tests.py --extreme  # Only on powerful dev machines
```

## 🛡️ **Test Reliability Measures**

### **Preventing CI Flakiness**
1. **Adaptive Timeouts**: Adjust based on system resources
2. **Retry Logic**: Limited retries for network-dependent operations
3. **Resource Detection**: Scale test parameters to available resources  
4. **Graceful Degradation**: Skip resource-intensive tests on constrained systems

### **Manual Test Safety**
1. **Resource Monitoring**: Warn when approaching system limits
2. **Graceful Shutdown**: Clean interruption handling
3. **System Adaptation**: Auto-adjust parameters based on detected resources
4. **Progressive Loading**: Start with light tests, increase load gradually

## 🎭 **Test Environment Matrix**

### **CI Environments** (GitHub Actions, etc.)
- **Resources**: Limited (2-4 cores, 2-8GB RAM)
- **Network**: May have restrictions or delays
- **Consistency**: Variable performance between runs
- **Test Types**: Unit + Light Integration only

### **Developer Machines**
- **Resources**: Variable (4-32 cores, 8-64GB RAM)  
- **Network**: Generally fast and stable
- **Consistency**: More predictable than CI
- **Test Types**: All test categories available

### **Production Staging**
- **Resources**: Production-equivalent
- **Network**: Production-equivalent
- **Consistency**: High
- **Test Types**: Full validation including extreme stress tests

## 📈 **Metrics and Monitoring**

### **CI Success Metrics**
- **Test Pass Rate**: > 99.5% over 30-day window
- **Build Time**: < 5 minutes end-to-end  
- **Resource Usage**: Peak memory < 1GB
- **Flakiness**: < 0.1% flaky test rate

### **Manual Test Insights**
- **Performance Trends**: Track throughput over time
- **Resource Efficiency**: Memory usage patterns
- **Stability Under Load**: Error rates at different load levels
- **System Limits**: Maximum sustainable connection counts

## 🔧 **Implementation Guidelines**

### **Writing CI-Safe Tests**
```python
# ✅ Good CI test characteristics
class TestConnectionManager(unittest.TestCase):
    def setUp(self):
        # Use small, predictable test data
        self.test_config = get_ci_safe_config()
        
    def test_ice_restart_policy(self):
        # Deterministic logic testing
        policy = ICERestartPolicy()
        self.assertTrue(policy.should_restart(mock_metrics, INTERFACE_CHANGE))
        
    def test_state_transitions(self):
        # Fast state machine verification
        manager = ConnectionManager()
        self.assertEqual(manager.state, ConnectionState.INITIALIZING)
```

### **Writing Manual Stress Tests**
```python
# ✅ Good manual stress test characteristics  
class ManualStressTest(unittest.TestCase):
    def setUp(self):
        # Adapt to system resources
        self.config = self._detect_system_config()
        self._warn_about_resource_usage()
        
    def test_high_load_cleanup(self):
        # Non-deterministic, resource-intensive testing
        connections = self._create_many_connections(self.config.max_connections)
        self._stress_test_cleanup(connections)
        
    def _detect_system_config(self):
        # Auto-adapt to available resources
        cpu_cores = os.cpu_count()
        memory_gb = get_available_memory_gb()
        return StressTestConfig.for_system(cpu_cores, memory_gb)
```

## 🎯 **Success Criteria**

### **Short-term Goals** (Next 3 months)
- [x] Separate CI and manual tests ✅
- [x] Reduce CI test runtime to < 5 minutes ✅  
- [x] Achieve > 99% CI test reliability ✅
- [x] Document testing strategy ✅

### **Long-term Goals** (Next year)
- [ ] Add chaos engineering tests
- [ ] Implement performance regression detection
- [ ] Add cross-platform testing matrices
- [ ] Develop automated performance benchmarking

## 📚 **References**

- **Test Implementation**: `tests/manual_stress_tests.py`
- **CI Configuration**: `.github/workflows/` (when implemented)
- **Performance Benchmarks**: `docs/HOT_PATH_OPTIMIZATION_SUMMARY.md`
- **Test Utils**: `tests/test_utils.py`

---

**🎯 This testing strategy ensures reliable CI builds while providing comprehensive manual testing capabilities for development and validation phases.**
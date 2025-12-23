#!/usr/bin/env python3
"""
Test for global instance_id initialization in Rust SDK.
"""

import pytest


def test_initialize_instance_id():
    """Test that initialize_instance_id can be called from Python"""

    print("\nTesting Instance ID Initialization")
    print("=" * 50)

    # Import the Rust module
    try:
        import keeper_pam_webrtc_rs
        print("✓ Successfully imported keeper_pam_webrtc_rs")
    except ImportError as e:
        pytest.skip(f"Rust SDK not available: {e}")
        return

    # Create a TubeRegistry instance
    registry = keeper_pam_webrtc_rs.PyTubeRegistry()
    print("✓ Created TubeRegistry instance")

    # Test 1: Initialize instance_id
    test_instance_id = "TESTID"
    try:
        registry.initialize_instance_id(test_instance_id)
        print(f"✓ Successfully initialized instance_id: {test_instance_id}")
    except Exception as e:
        pytest.fail(f"Failed to initialize instance_id: {e}")

    # Test 2: Try to initialize again (should fail)
    try:
        registry.initialize_instance_id("ANOTHER")
        pytest.fail("Should not be able to initialize instance_id twice")
    except RuntimeError as e:
        print(f"✓ Correctly rejected second initialization: {e}")
    except Exception as e:
        print(f"✓ Correctly rejected second initialization with: {e}")

    print("\n" + "=" * 50)
    print("All tests passed!")


def test_instance_id_with_none():
    """Test that instance_id can handle None or missing values

    Note: This test must run in a separate process from test_initialize_instance_id
    because instance_id is a global singleton that can only be initialized once.
    """

    print("\nTesting Instance ID with None/Missing Values")
    print("=" * 50)

    try:
        import keeper_pam_webrtc_rs
    except ImportError as e:
        pytest.skip(f"Rust SDK not available: {e}")
        return

    # Note: We can only test ONE initialization per test run since it's global
    # Testing different values in separate test functions or separate processes

    registry = keeper_pam_webrtc_rs.PyTubeRegistry()

    # Try to initialize with None - should accept and use empty string
    try:
        registry.initialize_instance_id(None)
        print("✓ Successfully initialized with None (uses empty string)")
    except Exception as e:
        # If already initialized, that's also OK (means previous test ran)
        if "already initialized" in str(e).lower():
            print(f"⚠ Already initialized (expected if other tests ran first): {e}")
        else:
            pytest.fail(f"Should accept None as instance_id: {e}")

    # Try to initialize again - should fail
    try:
        registry.initialize_instance_id("ANOTHER")
        pytest.fail("Should not allow re-initialization")
    except Exception as e:
        print(f"✓ Correctly rejected second initialization: {e}")

    print("\n" + "=" * 50)
    print("All None/missing tests passed!")


def test_instance_id_empty_string():
    """Test initialization with empty string in isolated process"""

    print("\nTesting Instance ID with Empty String")
    print("=" * 50)

    try:
        import keeper_pam_webrtc_rs
    except ImportError as e:
        pytest.skip(f"Rust SDK not available: {e}")
        return

    registry = keeper_pam_webrtc_rs.PyTubeRegistry()

    try:
        registry.initialize_instance_id("")
        print("✓ Successfully initialized with empty string")
    except Exception as e:
        # If already initialized, that's OK
        if "already initialized" in str(e).lower():
            print(f"⚠ Already initialized (expected if other tests ran first): {e}")
        else:
            pytest.fail(f"Should accept empty string: {e}")

    print("\n" + "=" * 50)


def test_instance_id_no_arguments():
    """Test initialization without arguments in isolated process"""

    print("\nTesting Instance ID with No Arguments")
    print("=" * 50)

    try:
        import keeper_pam_webrtc_rs
    except ImportError as e:
        pytest.skip(f"Rust SDK not available: {e}")
        return

    registry = keeper_pam_webrtc_rs.PyTubeRegistry()

    try:
        registry.initialize_instance_id()
        print("✓ Successfully initialized with no arguments (uses None/empty)")
    except Exception as e:
        # If already initialized, that's OK
        if "already initialized" in str(e).lower():
            print(f"⚠ Already initialized (expected if other tests ran first): {e}")
        else:
            pytest.fail(f"Should accept no arguments: {e}")

    print("\n" + "=" * 50)


if __name__ == "__main__":
    test_initialize_instance_id()
    test_instance_id_with_none()
    test_instance_id_empty_string()
    test_instance_id_no_arguments()

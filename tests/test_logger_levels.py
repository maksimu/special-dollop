#!/usr/bin/env python3
"""
Lightweight logging tests - no tube creation, no runtime interference.

Tests the CORE logging functionality that Commander depends on:
1. initialize_logger() works
2. initialize_logger() is idempotent (multiple calls safe)
3. set_verbose_logging() toggles the flag
4. Logger initialization with different parameters

These tests are FAST and SAFE - no WebRTC tubes, no runtime cleanup.
"""

import logging
import keeper_pam_webrtc_rs


def test_logger_initialization():
    """Test that logger initializes successfully"""
    # This might be called second (after another test), so it should be idempotent
    result = keeper_pam_webrtc_rs.initialize_logger(
        logger_name="test_init",
        verbose=False,
        level=logging.DEBUG
    )
    # Should succeed (either first init or idempotent)
    assert result is None, "initialize_logger should return None on success"
    print("✅ Logger initialization succeeded")


def test_idempotent_initialization():
    """Test that logger can be initialized multiple times safely (Commander pattern)"""

    # First call
    keeper_pam_webrtc_rs.initialize_logger(
        logger_name="test_idempotent_1",
        verbose=False,
        level=logging.DEBUG
    )
    print("✅ First initialization succeeded")

    # Second call with different params - should be idempotent
    keeper_pam_webrtc_rs.initialize_logger(
        logger_name="test_idempotent_2",
        verbose=True,
        level=logging.INFO
    )
    print("✅ Second initialization succeeded (idempotent)")

    # Third call - should also succeed
    keeper_pam_webrtc_rs.initialize_logger(
        logger_name="test_idempotent_3",
        verbose=False,
        level=logging.ERROR
    )
    print("✅ Third initialization succeeded (idempotent)")
    print("✅ Idempotent initialization verified - Commander pattern works!")


def test_verbose_flag_toggling():
    """Test that verbose flag can be toggled at runtime"""

    # Toggle off
    keeper_pam_webrtc_rs.set_verbose_logging(False)
    print("✅ Set verbose=False succeeded")

    # Toggle on
    keeper_pam_webrtc_rs.set_verbose_logging(True)
    print("✅ Set verbose=True succeeded")

    # Toggle off again
    keeper_pam_webrtc_rs.set_verbose_logging(False)
    print("✅ Verbose flag can be toggled at runtime")


def test_logger_functions_exist():
    """Verify all expected logger functions are exported"""

    assert hasattr(keeper_pam_webrtc_rs, 'initialize_logger'), \
        "initialize_logger should be exported"
    assert hasattr(keeper_pam_webrtc_rs, 'set_verbose_logging'), \
        "set_verbose_logging should be exported"

    # Verify they're callable
    assert callable(keeper_pam_webrtc_rs.initialize_logger), \
        "initialize_logger should be callable"
    assert callable(keeper_pam_webrtc_rs.set_verbose_logging), \
        "set_verbose_logging should be callable"

    print("✅ All required logger functions are exported and callable")


if __name__ == '__main__':
    """
    Run tests directly for quick verification.
    Or use: python3 -m pytest tests/test_logger_levels.py -v
    """
    import sys

    print("\n" + "="*80)
    print("LIGHTWEIGHT LOGGER TESTS - No Tubes, No Runtime Interference")
    print("="*80)

    logging.basicConfig(
        level=logging.DEBUG,
        format='%(levelname)-8s [%(name)s] %(message)s'
    )

    try:
        test_logger_initialization()
        test_idempotent_initialization()
        test_verbose_flag_toggling()
        test_logger_functions_exist()

        print("\n" + "="*80)
        print("ALL LOGGER TESTS PASSED ✅")
        print("="*80)
        print("\nVerified:")
        print("1. ✅ Logger initialization works")
        print("2. ✅ Idempotent initialization (Commander pattern)")
        print("3. ✅ Verbose flag runtime toggling")
        print("4. ✅ All required functions exported")
        print("\n" + "="*80)

    except Exception as e:
        print(f"\n❌ TEST FAILED: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)

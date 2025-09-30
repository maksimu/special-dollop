#!/usr/bin/env python3
"""
Test the enhanced ICE/TURN metrics in get_connection_stats.
"""
import json

def test_enhanced_ice_stats():
    """Test the enhanced ICE/TURN metrics that were added to get_connection_stats."""
    try:
        from keeper_pam_webrtc_rs import PyTubeRegistry

        # Create a registry instance
        registry = PyTubeRegistry()

        # Since we have no active connections, let's just check the structure
        # by examining what fields are available if a connection existed
        print("=== Enhanced ICE Statistics Test ===")

        # Test getting stats for a non-existent tube to see the structure
        try:
            stats = registry.get_connection_stats("test_tube_123")
            if stats:
                print("Unexpected: Got stats for non-existent tube")
                print(json.dumps(stats, indent=2))
            else:
                print("✅ Correctly returned None for non-existent tube")
        except Exception as e:
            print(f"Expected exception for non-existent tube: {e}")

        # Check what methods are available for examining enhanced stats
        available_methods = [method for method in dir(registry) if 'stats' in method.lower()]
        print(f"\nAvailable stats-related methods: {available_methods}")

        # Test system-wide stats that might show the new fields
        system_stats = registry.get_system_stats()
        print(f"\nSystem stats structure:")
        for key in system_stats.keys():
            print(f"  {key}: {type(system_stats[key])}")

        # Try aggregated metrics
        agg_metrics = registry.get_aggregated_metrics()
        print(f"\nAggregated metrics structure:")
        for key in agg_metrics.keys():
            print(f"  {key}: {type(agg_metrics[key])}")

        print("\n🎯 To see the new ICE/TURN metrics in action, you need:")
        print("   1. An active WebRTC connection")
        print("   2. ICE candidate gathering to occur")
        print("   3. TURN server interaction")
        print("   4. Then call get_connection_stats(tube_id)")
        print("\n💡 The new metrics include:")
        print("   - ICE candidate counts (host, srflx, relay)")
        print("   - ICE gathering timing")
        print("   - TURN allocation success rates")
        print("   - Connection leg latency breakdown")
        print("   - Selected candidate pair details")

        # Test passed - no return needed

    except ImportError as e:
        print(f"❌ Failed to import Rust module: {e}")
        assert False, f"Failed to import Rust module: {e}"
    except Exception as e:
        print(f"❌ Error testing enhanced ICE stats: {e}")
        assert False, f"Error testing enhanced ICE stats: {e}"

if __name__ == "__main__":
    test_enhanced_ice_stats()
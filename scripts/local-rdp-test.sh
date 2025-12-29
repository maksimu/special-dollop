#!/bin/bash
# Local RDP testing script
# This starts an xrdp Docker container for testing the RDP handler locally

set -e

CONTAINER_NAME="guacr-rdp-test"
IMAGE="danielguerra/ubuntu-xrdp:latest"
RDP_PORT=3389
TEST_USER="test_user"
TEST_PASS="test_password"

echo "=== Local RDP Test Environment ==="

# Function to cleanup
cleanup() {
    echo ""
    echo "Stopping container..."
    docker stop $CONTAINER_NAME 2>/dev/null || true
    docker rm $CONTAINER_NAME 2>/dev/null || true
    echo "Container stopped."
}

# Parse arguments
case "${1:-start}" in
    start)
        # Check if container is already running
        if docker ps -q -f name=$CONTAINER_NAME | grep -q .; then
            echo "Container $CONTAINER_NAME is already running."
            echo "RDP available at localhost:$RDP_PORT"
            echo "User: $TEST_USER / $TEST_PASS"
            echo ""
            echo "To stop: $0 stop"
            exit 0
        fi

        # Remove existing stopped container
        docker rm $CONTAINER_NAME 2>/dev/null || true

        echo "Pulling xrdp image (if needed)..."
        docker pull $IMAGE

        echo ""
        echo "Starting RDP container..."
        docker run -d \
            --name $CONTAINER_NAME \
            -p $RDP_PORT:3389 \
            -e TZ=UTC \
            $IMAGE

        echo ""
        echo "Waiting for container to start..."
        sleep 5

        echo "Creating test user..."
        docker exec $CONTAINER_NAME bash -c "
            useradd -m -s /bin/bash $TEST_USER 2>/dev/null || true
            echo '$TEST_USER:$TEST_PASS' | chpasswd
        "

        echo ""
        echo "Waiting for RDP service (xrdp takes ~30 seconds to fully start)..."
        for i in {1..30}; do
            if nc -z localhost $RDP_PORT 2>/dev/null; then
                echo "RDP service is ready!"
                break
            fi
            echo "  Waiting... ($i/30)"
            sleep 2
        done

        # Verify connection
        if nc -z localhost $RDP_PORT 2>/dev/null; then
            echo ""
            echo "=== RDP Test Server Ready ==="
            echo "Host: localhost"
            echo "Port: $RDP_PORT"
            echo "User: $TEST_USER"
            echo "Pass: $TEST_PASS"
            echo ""
            echo "To test manually with FreeRDP client:"
            echo "  xfreerdp /v:localhost:$RDP_PORT /u:$TEST_USER /p:$TEST_PASS /cert-ignore"
            echo ""
            echo "To run Rust integration tests:"
            echo "  RDP_TEST_HOST=localhost RDP_TEST_PORT=$RDP_PORT \\"
            echo "  RDP_TEST_USER=$TEST_USER RDP_TEST_PASS=$TEST_PASS \\"
            echo "  cargo test -p guacr-rdp --test integration_test -- --ignored"
            echo ""
            echo "To stop: $0 stop"
        else
            echo "ERROR: RDP service did not start"
            docker logs $CONTAINER_NAME
            cleanup
            exit 1
        fi
        ;;

    stop)
        cleanup
        ;;

    logs)
        docker logs -f $CONTAINER_NAME
        ;;

    shell)
        docker exec -it $CONTAINER_NAME /bin/bash
        ;;

    status)
        if docker ps -q -f name=$CONTAINER_NAME | grep -q .; then
            echo "Container is running"
            echo "RDP available at localhost:$RDP_PORT"
        else
            echo "Container is not running"
        fi
        ;;

    *)
        echo "Usage: $0 [start|stop|logs|shell|status]"
        echo ""
        echo "Commands:"
        echo "  start  - Start RDP test container (default)"
        echo "  stop   - Stop and remove container"
        echo "  logs   - Follow container logs"
        echo "  shell  - Open shell in container"
        echo "  status - Check if container is running"
        exit 1
        ;;
esac

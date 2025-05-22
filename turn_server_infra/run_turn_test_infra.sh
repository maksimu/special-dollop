#!/bin/bash
set -e # Exit immediately if a command exits with a non-zero status.
# set -x # Print commands and their arguments as they are executed (for debugging).

# This script demonstrates how to use the Taskfile to manage the TURN server infrastructure
# for automated testing.

# --- Configuration Variables ---
# REQUIRED: You must set these environment variables before running the script.
export TF_VAR_domain_name_prefix="rustunnel-$(date +%s)" # Example: Unique prefix for DNS
# TF_VAR_route53_zone_id is now auto-discovered by Terraform using TF_VAR_base_domain_name.

# OPTIONAL: Set these if you want to override defaults defined in variables.tf
# export TF_VAR_aws_region="us-west-2"
# export TF_VAR_instance_type="t2.micro"
# export TF_VAR_turn_username="myturnuser"
# export TF_VAR_turn_password="myturnpassword123"
export TF_VAR_base_domain_name="keeperpamlab.com" # Default is keeperpamlab.com; ensure this is correct for zone lookup.
# export TF_VAR_ssh_public_key_path="~/.ssh/another_key.pub"

# --- Helper Function for Cleanup ---
cleanup() {
    echo ""
    echo "--- Cleaning up infrastructure (task destroy) ---"
    if task destroy; then
        echo "Infrastructure destroyed successfully."
    else
        echo "ERROR: Failed to destroy infrastructure. Please check AWS console."
        # You might want to add more robust error handling/notification here for a CI environment
    fi
}

# --- Main Script Logic ---

echo "--- Configuration ---"
echo "Domain Name Prefix (TF_VAR_domain_name_prefix): ${TF_VAR_domain_name_prefix}"
echo "Base Domain Name (TF_VAR_base_domain_name):   ${TF_VAR_base_domain_name:-keeperpamlab.com} (Used for Route 53 Zone lookup)"
echo "Route 53 Zone ID:                            Auto-discovered by Terraform"
echo "Default TURN Username (from variables.tf):     turnuser (override with TF_VAR_turn_username)"
echo "Default TURN Password (from variables.tf):     turnpassword (override with TF_VAR_turn_password)"
echo "Default AWS Region (from variables.tf):        us-east-1 (override with TF_VAR_aws_region)"
echo "Default Instance Type (from variables.tf):     t3.small (override with TF_VAR_instance_type)"
echo "Default SSH Key Path (from variables.tf):      keys/vm_key.pub (override with TF_VAR_ssh_public_key_path)"

# Ensure TF_VAR_base_domain_name is set if not using the default explicitly in this script.
if [ -z "${TF_VAR_base_domain_name}" ]; then
    echo "Warning: TF_VAR_base_domain_name is not explicitly set, defaulting to 'keeperpamlab.com' for Route53 zone lookup."
    export TF_VAR_base_domain_name="keeperpamlab.com"
fi

# Ensure cleanup happens on script exit or interruption
trap cleanup EXIT SIGINT SIGTERM

# Navigate to the infrastructure directory
cd turn_server_infra || exit 1

echo ""
echo "--- Initializing Terraform (task init) ---"
if ! task init; then
    echo "ERROR: Terraform initialization failed."
    exit 1 # trap will call cleanup
fi

echo ""
echo "--- Applying Terraform configuration (task apply) ---"
echo "This may take a few minutes..."
if ! task apply; then
    echo "ERROR: Terraform apply failed."
    exit 1 # trap will call cleanup
fi

echo ""
echo "--- Fetching TURN server details (terraform output -json) ---"
# Using terraform output -json directly for easier parsing in scripts
TURN_DETAILS_JSON=$(terraform output -json)
if [ $? -ne 0 ]; then
    echo "ERROR: Failed to get Terraform outputs."
    exit 1 # trap will call cleanup
fi

echo "TURN Server Details (JSON):"
echo "${TURN_DETAILS_JSON}"

# Example of how to parse with 'jq' (ensure jq is installed: sudo yum install jq / sudo apt install jq / brew install jq)
# PUBLIC_IP=$(echo "${TURN_DETAILS_JSON}" | jq -r '.turn_server_public_ip.value')
# HOSTNAME=$(echo "${TURN_DETAILS_JSON}" | jq -r '.turn_server_hostname.value')
# USERNAME=$(echo "${TURN_DETAILS_JSON}" | jq -r '.turn_server_username.value')
# PASSWORD=$(echo "${TURN_DETAILS_JSON}" | jq -r '.turn_server_password.value') # Be careful with sensitive data
# REALM=$(echo "${TURN_DETAILS_JSON}" | jq -r '.turn_server_realm.value')

# echo ""
# echo "Parsed details:"
# echo "Public IP: ${PUBLIC_IP}"
# echo "Hostname:  ${HOSTNAME}"
# echo "Username:  ${USERNAME}"
# echo "Password:  ${PASSWORD}" # Consider not printing this directly
# echo "Realm:     ${REALM}"

echo ""
echo "--- (Simulating tests using the TURN server) ---"
# Replace this with your actual test execution command
# Example: your_rust_test_binary --turn-url "turn:${HOSTNAME}:3478" --turn-username "${USERNAME}" --turn-password "${PASSWORD}" --turn-realm "${REALM}"
sleep 30 # Simulate test duration
echo "--- (Test simulation complete) ---"

# The cleanup function will be called automatically on exit.

echo ""
echo "Script finished. Infrastructure will be destroyed by cleanup function." 
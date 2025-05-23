output "turn_server_public_ip" {
  description = "Public IP address of the TURN server."
  value       = aws_eip.webrtc_eip.public_ip
}

output "turn_server_hostname" {
  description = "Hostname of the TURN server."
  value       = aws_route53_record.turn_server_dns.fqdn
}

output "turn_server_credential_info" {
  description = "Information about TURN server dynamic credentials (static username/password not used)."
  value = "TURN server uses dynamic authentication with shared secret. Use './test_turn_credentials.sh' or generate manually with HMAC-SHA1."
}

output "turn_server_auth_secret" {
  description = "Authentication secret for the TURN server (used with use-auth-secret)."
  value       = var.turn_auth_secret
  sensitive   = false
}

output "turn_server_generate_credentials" {
  description = "Generate TURN credentials (48h default, with options)."
  value = "./test_turn_credentials.sh"
}

output "turn_server_test_with_credentials" {
  description = "Generate credentials and test TURN server."
  value = "./test_turn_credentials.sh --test"
}

output "turn_server_realm" {
  description = "Realm for the TURN server."
  value       = local.full_domain_name
}

output "turn_server_instance_id" {
  description = "ID of the EC2 instance running the TURN server."
  value       = aws_instance.webrtc_server.id
} 
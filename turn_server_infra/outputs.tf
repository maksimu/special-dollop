output "turn_server_public_ip" {
  description = "Public IP address of the TURN server."
  value       = aws_eip.webrtc_eip.public_ip
}

output "turn_server_hostname" {
  description = "Hostname of the TURN server."
  value       = aws_route53_record.turn_server_dns.fqdn
}

output "turn_server_username" {
  description = "Username for the TURN server."
  value       = var.turn_username
}

output "turn_server_password" {
  description = "Password for the TURN server."
  value       = var.turn_password
  sensitive   = true
}

output "turn_server_realm" {
  description = "Realm for the TURN server."
  value       = local.full_domain_name
}

output "turn_server_instance_id" {
  description = "ID of the EC2 instance running the TURN server."
  value       = aws_instance.webrtc_server.id
} 
data "aws_route53_zone" "selected" {
  name         = "${var.base_domain_name}." # Ensure trailing dot for FQDN
  private_zone = false
}

resource "random_string" "dns_suffix" {
  length  = 8
  special = false
  upper   = false
}

locals {
  dynamic_hostname = "${var.domain_name_prefix}-${random_string.dns_suffix.result}"
  full_domain_name = "${local.dynamic_hostname}.${var.base_domain_name}"
}

resource "aws_route53_record" "turn_server_dns" {
  zone_id = data.aws_route53_zone.selected.zone_id # Use the looked-up zone ID
  name    = local.full_domain_name
  type    = "A"
  ttl     = 300
  records = [aws_eip.webrtc_eip.public_ip]
} 
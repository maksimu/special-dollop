variable "aws_region" {
  description = "AWS region to deploy resources"
  type        = string
  default     = "us-east-1" 
}

variable "instance_type" {
  description = "EC2 instance type for the WebRTC relay server"
  type        = string
  default     = "t3.small"
}

variable "turn_username" {
  description = "Username for TURN server authentication"
  type        = string
  default     = "turnuser"
}

variable "turn_password" {
  description = "Password for TURN server authentication"
  type        = string
  default     = "turnpassword"
  sensitive   = true
}

variable "domain_name_prefix" {
  description = "Prefix for the domain name (e.g., rustunnel-test-[timestamp])"
  type        = string
}

variable "base_domain_name" {
  description = "Base domain name (e.g., keeperpamlab.com)"
  type        = string
  default     = "keeperpamlab.com"
}

variable "route53_zone_id" {
  description = "Route53 hosted zone ID for the domain. This is typically auto-discovered based on base_domain_name. Provide only if specific lookup is needed or auto-discovery fails."
  type        = string
  default     = "" # No longer strictly required as input, will be looked up
}

variable "creator_tag" {
  description = "Tag to identify the creator of these resources."
  type        = string
  default     = "rustunnel-test-automation"
}

variable "ssh_public_key_path" {
  description = "Path to the SSH public key for the EC2 instance."
  type        = string
  default     = "keys/vm_key.pub" # Updated to use the key within the project
} 
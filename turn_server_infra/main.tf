terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
  required_version = ">= 1.2.0"
}

provider "aws" {
  region = var.aws_region
}

# Get latest Amazon Linux 2023 AMI
data "aws_ami" "amazon_linux_2023" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-*-x86_64"]
  }

  filter {
    name   = "virtualization-type"
    values = ["hvm"]
  }
}

# VPC for the WebRTC relay server
resource "aws_vpc" "webrtc_vpc" {
  cidr_block           = "10.0.0.0/16"
  enable_dns_hostnames = true
  enable_dns_support   = true

  tags = {
    Name = "webrtc-vpc"
  }
}

# Internet Gateway for public subnet
resource "aws_internet_gateway" "webrtc_igw" {
  vpc_id = aws_vpc.webrtc_vpc.id

  tags = {
    Name = "webrtc-igw"
  }
}

# Public subnet
resource "aws_subnet" "webrtc_public_subnet" {
  vpc_id                  = aws_vpc.webrtc_vpc.id
  cidr_block              = "10.0.1.0/24"
  map_public_ip_on_launch = true
  availability_zone       = "${var.aws_region}a" # Consider making this configurable or dynamic

  tags = {
    Name = "webrtc-public-subnet"
  }
}

# Route table for public subnet
resource "aws_route_table" "webrtc_public_route" {
  vpc_id = aws_vpc.webrtc_vpc.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.webrtc_igw.id
  }

  tags = {
    Name = "webrtc-public-route"
  }
}

# Associate public subnet with route table
resource "aws_route_table_association" "webrtc_public_association" {
  subnet_id      = aws_subnet.webrtc_public_subnet.id
  route_table_id = aws_route_table.webrtc_public_route.id
}

# Security group for the WebRTC relay server
resource "aws_security_group" "webrtc_sg" {
  name        = "webrtc-security-group"
  description = "Security group for WebRTC relay server"
  vpc_id      = aws_vpc.webrtc_vpc.id

  # SSH access
  ingress {
    from_port   = 22
    to_port     = 22
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "SSH"
  }

  # Standard STUN/TURN TCP
  ingress {
    from_port   = 3478
    to_port     = 3478
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "STUN/TURN TCP"
  }

  # Standard STUN/TURN UDP
  ingress {
    from_port   = 3478
    to_port     = 3478
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "STUN/TURN UDP"
  }

  # TLS STUN/TURN TCP
  ingress {
    from_port   = 5349
    to_port     = 5349
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "TLS STUN/TURN"
  }

  # TLS STUN/TURN UDP
  ingress {
    from_port   = 5349
    to_port     = 5349
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "TLS STUN/TURN UDP"
  }

  # TURN relay ports UDP (e.g., 49152-49162, can be adjusted)
  ingress {
    from_port   = 49152
    to_port     = 65535 # Standard range for TURN relay ports
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
    description = "TURN Relay Ports"
  }

  # Allow all outbound traffic
  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    Name = "webrtc-sg"
  }
}

locals {
  # turn_user_credentials = "${var.turn_username}:${var.turn_password}" # Removed for open TURN server
}

# Create key pair using the provided public key
resource "aws_key_pair" "webrtc_key_pair" { # Renamed from webrtc_key_pair2 for simplicity
  key_name   = "webrtc-relay-key" # Renamed for simplicity
  public_key = file(var.ssh_public_key_path)
  
  tags = {
    Name      = "webrtc-relay-key"
    Creator   = var.creator_tag # Added variable for creator tag
    ManagedBy = "terraform"
  }
}

# IAM Role for EC2 instance to allow SSM
resource "aws_iam_role" "ec2_ssm_role" {
  name = "EC2SSMRoleForTURNServer"
  assume_role_policy = jsonencode({
    Version = "2012-10-17",
    Statement = [
      {
        Action = "sts:AssumeRole",
        Effect = "Allow",
        Principal = {
          Service = "ec2.amazonaws.com"
        }
      }
    ]
  })

  tags = {
    Name = "EC2SSMRoleForTURNServer"
  }
}

resource "aws_iam_role_policy_attachment" "ssm_policy_attachment" {
  role       = aws_iam_role.ec2_ssm_role.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "ec2_instance_profile" {
  name = "TURNServerInstanceProfile"
  role = aws_iam_role.ec2_ssm_role.name
}

# EC2 instance for the WebRTC relay server
resource "aws_instance" "webrtc_server" {
  ami                         = data.aws_ami.amazon_linux_2023.id
  instance_type               = var.instance_type
  key_name                    = aws_key_pair.webrtc_key_pair.key_name
  subnet_id                   = aws_subnet.webrtc_public_subnet.id
  vpc_security_group_ids      = [aws_security_group.webrtc_sg.id]
  associate_public_ip_address = true # Already true due to subnet, but explicit
  iam_instance_profile        = aws_iam_instance_profile.ec2_instance_profile.name # Added instance profile

  # User data script to install and configure coturn
  user_data = templatefile("${path.module}/scripts/configure-coturn.sh.tpl", {
    # turn_user_credentials  = local.turn_user_credentials # Removed for open TURN server
    turn_realm             = local.full_domain_name # Use the dynamically generated domain name, correctly referencing local
    coturn_version_tf_var  = "4.6.2" # Added coturn version
  })

  tags = {
    Name = "webrtc-relay-server"
  }
}

# Elastic IP for the WebRTC relay server
resource "aws_eip" "webrtc_eip" {
  instance = aws_instance.webrtc_server.id
  domain   = "vpc"

  tags = {
    Name = "webrtc-eip"
  }
} 
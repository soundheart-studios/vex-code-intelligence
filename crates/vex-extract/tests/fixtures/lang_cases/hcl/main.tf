provider "aws" { region = var.region }
variable "region" { default = "us-east-1" }
locals {
  name = "app-${var.env}"
  tags = { owner = local.name }
}
resource "aws_s3_bucket" "logs" {
  bucket = local.name
  count  = var.n
  depends_on = [module.vpc, data.aws_iam_policy.p, aws_s3_bucket.other]
}
data "aws_iam_policy" "p" { arn = each.value }
module "vpc" {
  source = "./modules/vpc"
  cidr   = var.cidr
}
output "bucket" { value = aws_s3_bucket.logs.id }
terraform { required_version = ">= 1.0" }

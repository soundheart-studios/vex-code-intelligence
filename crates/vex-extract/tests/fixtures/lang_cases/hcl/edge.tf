output "a" { value = data.foo[0].bar }
resource "aws_x" "y" {
  source = "not-a-module"
  v = var.list[0].x
}

# comment line
first() { :; }
source ''
source "lib&x.sh"
function a$b { :; }
x=$(source ./lib/util.sh)
first
a\;b() { :; }

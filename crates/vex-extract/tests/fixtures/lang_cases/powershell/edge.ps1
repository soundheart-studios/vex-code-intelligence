Import-Module -name Pester
function Run-All { Invoke-Everything }
function Outer { function Inner { Get-A }; Get-B }
class W { [string] R([string]$x = (Get-Default)) { return (Get-Body) } }
function F { process-items }
Import-Module .\lib\My.Module.psm1

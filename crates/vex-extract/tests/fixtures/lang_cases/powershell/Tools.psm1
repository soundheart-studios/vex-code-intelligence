using module .\Helpers.psm1
using namespace System.IO
Import-Module -Name Pester
Import-Module .\lib\Util.psm1
. .\common.ps1

function Get-Thing {
    param($Name)
    $x = Invoke-Helper -Name $Name
    if ($x) { Write-Output $x }
    get-thing2
}

function Get-Thing2 { return 1 }

class Widget {
    [string] Render() {
        return (Get-Thing -Name "w")
    }
}

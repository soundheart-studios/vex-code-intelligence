@{
    RootModule = 'Tools.psm1'
    NestedModules = @('Sub\Nested.psm1', 'Other.dll')
    RequiredModules = @('Pester', @{ ModuleName = 'PSReadLine'; ModuleVersion = '2.0' })
    ModuleVersion = '1.0'
}

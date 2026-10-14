package demo
import org.gradle.api.DefaultTask
import groovy.transform.CompileStatic

class Generator extends DefaultTask {
    String name = defaultName
    @Override
    String toString() { "Generator(${name})" }
    def run(List items) {
        items.each { process(it) }
        helper.go()
    }
    private static int count(String s) { s.size() }
    Generator(String n) { this.name = n }
    class Nested { def inner() { outer() } }
}

class FeatureSpec extends Specification {
    def "reads the data"() {
        expect: load() == 1
    }
    def 'single quoted'() { when: run() }
}

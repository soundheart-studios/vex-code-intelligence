class Message {
    fun configure() {
        sealed = "foo"
        open += 1
        by = 3
        val same = sealed == "foo"
    }
    fun after(): Int = compute()
}

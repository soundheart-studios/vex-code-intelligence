package demo

fun String.shout(): String = uppercase() + helper(2)

fun List<Int>.total(): Int = sum()

class Box {
    fun Int.double(): Int = this * 2
}

fun helper(x: Int): Int = x

object Registry { fun all() = emptyList() }

package demo

import kotlin.collections.List
import java.io.Closeable as C

class Proto {
    val name: String = "n"
    fun build(): Int = helper(1)
    companion object {
        fun create() = Proto()
    }
}

object Registry { fun all() = listOf<Int>() }

fun String.shout(): String = uppercase() + helper(2)

fun helper(x: Int): Int = x

interface Shape { fun area(): Double }

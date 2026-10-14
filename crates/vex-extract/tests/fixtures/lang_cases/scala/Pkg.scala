package demo

import scala.collection.mutable.{Map, Set}
import java.util._
import cats.effect.IO

trait Service {
  def exitCode(): Int
  def run(args: List[String]): Unit = helper(args.length)
}

object Main extends Service {
  def exitCode(): Int = 0
  private def helper(n: Int): Unit = println(n)
  @inline def twice(x: Int) = x * 2
}

class Box[T](val v: T) { def map[U](f: T => U): Box[U] = new Box(f(v)) }

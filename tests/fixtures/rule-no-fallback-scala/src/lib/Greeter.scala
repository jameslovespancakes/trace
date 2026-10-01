package lib

class Greeter {
  def greet(name: String): String = "hi " + name
}

object Greeter {
  def format(s: String): String = s.trim
}

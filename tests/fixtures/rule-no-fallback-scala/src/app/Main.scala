package app

import lib.Greeter

object Main {
  def run(greeter: Greeter): String = {
    val label = Greeter.format("x")
    greeter.greet(label)
  }
}

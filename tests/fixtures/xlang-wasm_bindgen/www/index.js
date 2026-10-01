// Fixture (P5, wasm_bindgen): JavaScript using the generated `game-core` package.
import { Universe, Cell, greetUser, render } from "game-core";
import { helper } from "./local.js";

export function main() {
  const universe = Universe.new(); // proven -> Universe::new
  universe.tick(); // instance of the exported class (new() -> Universe): proven -> Universe::tick
  greetUser("x"); // proven -> greet_user (js_name)
  render(); // two crates export `render` -> possible
  helper(); // negative control: local JavaScript module
  if (universe.alive() === Cell.Alive) { // variant read -> proven Cell (Alive); alive() is not exported
    return;
  }
}

export function shadowed(universe) {
  universe.tick(); // negative control: the parameter rebinds `universe`
}

export function rebound() {
  let board = Universe.new(); // bound twice: its type is unknown, no instance bridge
  board = helper();
  board.tick();
}

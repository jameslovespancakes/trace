// Fixture (P5, wasm_bindgen): negative control, same-named local functions.
export function helper() {
  return greetUser();
}

export function greetUser() {
  return "local";
}

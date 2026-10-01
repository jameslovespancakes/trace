// Fixture (P5, wasm_bindgen): exports of the `game-core` crate used from www/index.js.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Universe {
    width: u32,
}

/// Exported C-style enum: JavaScript sees `Cell.Dead` / `Cell.Alive`.
#[wasm_bindgen]
#[repr(u8)]
pub enum Cell {
    Dead = 0,
    Alive = 1,
}

#[wasm_bindgen]
impl Universe {
    /// Unique: `Universe.new()` in JavaScript -> proven; returns the exported class.
    pub fn new() -> Universe {
        Universe { width: 64 }
    }

    pub fn tick(&mut self) {
        self.width += 0;
    }

    /// Not exported (private): never a bridge target.
    fn private_helper(&self) -> u32 {
        self.width
    }
}

impl Universe {
    /// Plain impl block (no #[wasm_bindgen]): not visible to JavaScript.
    pub fn alive(&self) -> Cell {
        Cell::Alive
    }
}

/// Exported under its `js_name` -> proven from `greetUser(...)`.
#[wasm_bindgen(js_name = greetUser)]
pub fn greet_user(name: &str) -> String {
    name.to_string()
}

/// Ambiguous: other/src/lib.rs exports `render` as well -> possible.
#[wasm_bindgen]
pub fn render() {}

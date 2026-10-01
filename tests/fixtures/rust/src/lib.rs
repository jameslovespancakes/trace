//! Fixture crate for trace integration tests (never built).

pub trait Shape {
    fn area(&self) -> f64;
}

pub struct Circle {
    pub radius: f64,
}

impl Shape for Circle {
    fn area(&self) -> f64 {
        square(self.radius) * 3.14
    }
}

pub struct Rect {
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(w: f64, h: f64) -> Self {
        Rect { w, h }
    }
}

impl Shape for Rect {
    fn area(&self) -> f64 {
        self.w * self.h
    }
}

fn square(x: f64) -> f64 {
    x * x
}

pub fn total_area(shapes: &[&dyn Shape]) -> f64 {
    let mut sum = 0.0;
    for s in shapes {
        sum += s.area();
    }
    sum
}

pub fn demo() -> f64 {
    let rect = Rect::new(2.0, 3.0);
    let circle = Circle { radius: 1.0 };
    total_area(&[&circle, &rect])
}

/// Generic impl with an associated function called through a type path.
pub enum Data<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}

impl<'a> Data<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Data<'a> {
        match std::str::from_utf8(bytes) {
            Ok(text) => Data::Text(text),
            Err(_) => Data::Bytes(bytes),
        }
    }
}

pub fn decode(bytes: &[u8]) -> usize {
    match Data::from_bytes(bytes) {
        Data::Text(text) => text.len(),
        Data::Bytes(raw) => raw.len(),
    }
}

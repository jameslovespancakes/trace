use std::fmt::Debug;

pub struct JoinHandle<T>(T);

pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    unimplemented!()
}

pub fn show<T: Debug>(t: T) {}

pub fn call_boxed(f: Box<dyn Fn(i32) -> i32>) {}

pub struct Once;

impl Once {
    pub fn call_once<F: FnOnce()>(&self, f: F) {}
}

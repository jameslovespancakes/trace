pub fn each<F: Fn(i32)>(xs: &[i32], f: F) {
    for x in xs {
        f(*x);
    }
}

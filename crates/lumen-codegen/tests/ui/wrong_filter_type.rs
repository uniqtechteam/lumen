#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

fn main() {
    // `email` is text: a number does not fit
    let _ = db::user::email::equals(5);
}

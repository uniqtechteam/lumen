#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

fn main() {
    // `id` is a number: text does not fit
    let _ = db::user::id::equals("1");
}

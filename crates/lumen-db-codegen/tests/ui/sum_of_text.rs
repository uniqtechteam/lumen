#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

fn main() {
    // `title` is text: it has no sum
    let _ = db::post::title::sum();
}

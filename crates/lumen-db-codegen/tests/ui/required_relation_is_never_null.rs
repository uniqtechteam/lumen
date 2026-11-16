#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

fn main() {
    // every post has an author, so `is_null` is not offered on `post::author`
    let _ = db::post::author::is_null();
}

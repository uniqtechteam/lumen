#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

fn main() {
    // a post created under its author gets the key from the nested write, so it has no field for it
    let _ = db::post::CreateWithoutAuthor {
        id: None,
        title: "Hello".into(),
        body: None,
        published: None,
        views: None,
        author_id: 1,
    };
}

#[rustfmt::skip]
#[path = "../generated/blog.rs"]
#[allow(dead_code)]
mod db;

async fn run(client: db::Client) {
    // a value of `User` does not fit an aggregate over `Post`
    let _ = client.post().aggregate().value(db::user::id::sum());
}

fn main() {
    let _ = run;
}

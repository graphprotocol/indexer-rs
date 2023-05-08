use std::env;
use lazy_static::lazy_static;

lazy_static! {
    pub static ref DATABASE_URL: String = env::var("DATABASE_URL")
        .expect("DATABASE_URL is not set")
        .to_string();
}

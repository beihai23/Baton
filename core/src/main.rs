//! baton-core — 独立 HTTP server 入口（开发/无桌面壳时使用）

fn main() {
    let db_path = baton_core::default_db_path();
    let addr = std::env::var("BATON_ADDR").unwrap_or_else(|_| "127.0.0.1:7700".into());
    baton_core::server::serve(&addr, &db_path);
}

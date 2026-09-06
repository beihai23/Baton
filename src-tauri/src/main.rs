//! Baton Desktop — Tauri 壳：启动内嵌 core server，WebView 加载前端。
//!
//! 服务进程策略：启动时先探测 127.0.0.1:7700 是否已有 Baton 在听（WebUI 模式）
//! —— 有则复用（不内嵌启动，两边同库同服务）；没有才 spawn 内嵌线程。
//! 数据库路径与所有入口统一（baton_core::default_db_path：~/.baton/baton.db，
//! BATON_DB 可覆盖）——Tauri / WebUI / CLI / MCP 共享同一数据世界。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// 探测 7700 上是否已有 Baton 服务（std 实现，零依赖）：GET /api/v1/board 应 200
fn baton_already_running() -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(
        &"127.0.0.1:7700".parse().unwrap(),
        Duration::from_millis(300),
    ) else {
        return false;
    };
    s.set_read_timeout(Some(Duration::from_millis(800))).ok();
    if s.write_all(b"GET /api/v1/board HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    matches!(s.read(&mut buf), Ok(n) if String::from_utf8_lossy(&buf[..n]).contains("200"))
}

fn main() {
    tauri::Builder::default()
        .setup(|_app| {
            if baton_already_running() {
                println!("baton-core 已在 127.0.0.1:7700 运行，复用现有服务（不内嵌启动）");
                return Ok(());
            }
            // 内嵌 core server：与 CLI/MCP 共享同一套 Db 逻辑
            let db = baton_core::default_db_path();
            std::thread::spawn(move || {
                baton_core::server::serve("127.0.0.1:7700", &db);
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Baton");
}

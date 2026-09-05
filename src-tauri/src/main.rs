//! Baton Desktop — Tauri 壳：启动内嵌 core server，WebView 加载前端。

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // 内嵌 core server：与 CLI/MCP 共享同一套 Db 逻辑
            let dir = app.path().app_data_dir().expect("no app data dir");
            std::fs::create_dir_all(&dir).ok();
            let db = dir.join("baton.db");
            std::thread::spawn(move || {
                baton_core::server::serve("127.0.0.1:7700", db.to_str().unwrap());
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Baton");
}

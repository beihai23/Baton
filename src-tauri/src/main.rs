fn main() {
    tauri::Builder::default()
        .setup(|_app| {
            // TODO(v0.1 后半段): 在此启动内嵌 baton-core（spawn 线程跑 HTTP server），
            // WebView 加载前端后通过 http://127.0.0.1:7700 与 core 通信。
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Baton");
}

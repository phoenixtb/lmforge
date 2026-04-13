// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|_app| {
            tauri::async_runtime::spawn(async move {
                // Initialize background process exactly like `lmforge start --foreground`
                use clap::Parser;
                let args = lmforge::cli::Cli::parse_from(["lmforge", "start", "--foreground"]);
                
                // Initialize logging for the background daemon
                // Skip if it fails (sometimes Tauri setups init logs already)
                let _ = lmforge::logging::init(&args);
                
                if let Ok(config) = lmforge::config::load(&args) {
                    println!("🚀 Starting LMForge Orchestrator embedded in Tauri...");
                    if let Err(e) = lmforge::cli::dispatch(args, config).await {
                        eprintln!("❌ Embedded LMForge crashed: {}", e);
                    }
                } else {
                    eprintln!("❌ Failed to load LMForge config from Tauri.");
                }
            });
            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

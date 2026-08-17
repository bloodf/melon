mod controller;
mod config;
mod runtime;
mod process_tree;
mod runtime_seed;

use controller::{ConnectionController, activate, probe, resolve_harness_root, resolve_node_sidecar, shutdown, status};

fn configure<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(ConnectionController::default())
        .setup(|app| {
            use tauri::Manager;
            let app_data = app.path().app_data_dir().unwrap_or_default();
            let resource_dir = app.path().resource_dir().unwrap_or_default();
            let exe_dir = std::env::current_exe().ok().and_then(|path| path.parent().map(std::path::Path::to_path_buf));
            app.state::<ConnectionController>().bind_runtime(
                app_data,
                resolve_harness_root(&resource_dir),
                resolve_node_sidecar(&resource_dir, exe_dir.as_deref()),
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![status, probe, activate, shutdown])
}

pub fn run() {
    use tauri::Manager;
    let app = configure(tauri::Builder::default())
        .build(tauri::generate_context!())
        .expect("failed to build Melon");
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let _ = handle.state::<ConnectionController>().shutdown_terminal();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::configure;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder};
    use tauri::webview::InvokeRequest;
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    fn request(url: &str) -> InvokeRequest {
        InvokeRequest {
            cmd: "status".into(),
            callback: CallbackFn(0),
            error: CallbackFn(1),
            url: url.parse().expect("valid test URL"),
            body: InvokeBody::default(),
            headers: Default::default(),
            invoke_key: INVOKE_KEY.into(),
        }
    }

    #[test]
    fn setup_window_can_invoke_registered_status() {
        let app = configure(mock_builder())
            .build(tauri::generate_context!())
            .expect("mock app");
        let setup = WebviewWindowBuilder::new(&app, "setup", WebviewUrl::default())
            .build()
            .expect("setup window");
        let response = get_ipc_response(&setup, request("tauri://localhost"))
            .expect("setup status response")
            .deserialize::<serde_json::Value>()
            .expect("status JSON");
        assert_eq!(response, serde_json::json!({ "keyPersistenceAvailable": false, "running": false }));
    }

    #[test]
    fn remote_main_window_cannot_invoke_setup_commands() {
        let app = configure(mock_builder())
            .build(tauri::generate_context!())
            .expect("mock app");
        let main = WebviewWindowBuilder::new(
            &app,
            "main",
            WebviewUrl::External("http://127.0.0.1:3080".parse().expect("remote URL")),
        )
        .build()
        .expect("main window");
        let error = get_ipc_response(&main, request("http://127.0.0.1:3080"))
            .expect_err("remote Harness status must be denied");
        assert!(error.to_string().contains("not allowed"), "unexpected denial: {error}");
    }
}

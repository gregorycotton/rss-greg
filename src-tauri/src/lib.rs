use std::{
    fs::OpenOptions,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use tauri::Manager;

const BACKEND_HOST: &str = "127.0.0.1";
const BACKEND_PORT: u16 = 5123;

struct BackendProcess(Mutex<Option<Child>>);

impl Drop for BackendProcess {
    fn drop(&mut self) {
        if let Ok(mut child) = self.0.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let mut backend = start_backend_if_needed()?;
            wait_for_backend(backend.as_mut())?;

            app.manage(BackendProcess(Mutex::new(backend)));

            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or("missing main window config")?;

            tauri::WebviewWindowBuilder::from_config(app.handle(), window_config)?.build()?;

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}

fn start_backend_if_needed() -> Result<Option<Child>, Box<dyn std::error::Error>> {
    if backend_is_healthy() {
        return Ok(None);
    }

    let project_root = find_project_root()?;
    let nix_shell = find_nix_shell()?;
    let log_path = std::env::temp_dir().join("gregs-feed-backend.log");
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let stderr = stdout.try_clone()?;

    let child = Command::new(nix_shell)
        .current_dir(project_root)
        .env("RSS_FEED_HOST", BACKEND_HOST)
        .env("RSS_FEED_PORT", BACKEND_PORT.to_string())
        .env("RSS_FEED_DEBUG", "0")
        .arg("--run")
        .arg("python app.py")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()?;

    Ok(Some(child))
}

fn wait_for_backend(backend: Option<&mut Child>) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backend = backend;

    while Instant::now() < deadline {
        if backend_is_healthy() {
            return Ok(());
        }

        if let Some(child) = backend.as_deref_mut() {
            if let Some(status) = child.try_wait()? {
                return Err(format!("backend exited before it was ready: {status}").into());
            }
        }

        thread::sleep(Duration::from_millis(250));
    }

    Err("backend did not become ready within 30 seconds".into())
}

fn backend_is_healthy() -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], BACKEND_PORT));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(200)) else {
        return false;
    };

    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let request = format!(
        "GET /api/health HTTP/1.1\r\nHost: {BACKEND_HOST}:{BACKEND_PORT}\r\nConnection: close\r\n\r\n"
    );

    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200")
}

fn find_project_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut candidates = Vec::new();

    if let Ok(root) = std::env::var("GREGS_FEED_ROOT") {
        candidates.push(PathBuf::from(root));
    }

    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.clone());
        if let Some(parent) = current_dir.parent() {
            candidates.push(parent.to_path_buf());
        }
    }

    if let Some(parent) = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent() {
        candidates.push(parent.to_path_buf());
    }

    candidates
        .into_iter()
        .find(|path| path.join("app.py").is_file() && path.join("shell.nix").is_file())
        .ok_or_else(|| "could not locate backend project root".into())
}

fn find_nix_shell() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut candidates = vec![
        PathBuf::from("/run/current-system/sw/bin/nix-shell"),
        PathBuf::from("/nix/var/nix/profiles/default/bin/nix-shell"),
        PathBuf::from("nix-shell"),
    ];

    if let Ok(home) = std::env::var("HOME") {
        candidates.insert(2, PathBuf::from(home).join(".nix-profile/bin/nix-shell"));
    }

    candidates
        .into_iter()
        .find(|path| path.is_file() || path == &PathBuf::from("nix-shell"))
        .ok_or_else(|| "could not locate nix-shell".into())
}

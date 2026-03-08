#![cfg(target_os = "windows")]
#![allow(dead_code)]

use std::{
    env,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

pub const CONFIG_FILE_NAME: &str = "iobitunlocker_shell_bridge.ini";
pub const HELPER_EXE_NAME: &str = "iobitunlocker_shell_bridge_helper.exe";
pub const WORKER_DLL_NAME: &str = "iobitunlocker_shell_bridge_worker.dll";
pub const LOG_FILE_NAME: &str = "iobitunlocker_shell_bridge.log";

const DEFAULT_ROOT_MENU_TEXT: &str = "Iobit Unlocker快捷操作";
const DEFAULT_UNLOCK_MENU_TEXT: &str = "解锁";
const DEFAULT_DELETE_MENU_TEXT: &str = "解锁并删除";
const DEFAULT_DEBUG_LOG: bool = false;
const DEFAULT_UNLOCK_FORCE_FALLBACK: bool = false;
const DEFAULT_DELETE_FORCE_FALLBACK: bool = false;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeAction {
    Unlock,
    Delete,
}

impl BridgeAction {
    pub fn from_command_id(id: usize) -> Option<Self> {
        match id {
            0 => Some(Self::Unlock),
            1 => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn as_config_value(self) -> &'static str {
        match self {
            Self::Unlock => "unlock",
            Self::Delete => "delete",
        }
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "unlock" => Some(Self::Unlock),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn operation_code(self) -> u32 {
        match self {
            Self::Unlock => 0,
            Self::Delete => 1,
        }
    }

    pub fn cli_switch(self) -> &'static str {
        match self {
            Self::Unlock => "/None",
            Self::Delete => "/Delete",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MenuTextConfig {
    pub root_menu_text: String,
    pub unlock_menu_text: String,
    pub delete_menu_text: String,
    pub debug_log: bool,
    pub unlock_force_fallback: bool,
    pub delete_force_fallback: bool,
}

impl Default for MenuTextConfig {
    fn default() -> Self {
        Self {
            root_menu_text: DEFAULT_ROOT_MENU_TEXT.to_string(),
            unlock_menu_text: DEFAULT_UNLOCK_MENU_TEXT.to_string(),
            delete_menu_text: DEFAULT_DELETE_MENU_TEXT.to_string(),
            debug_log: DEFAULT_DEBUG_LOG,
            unlock_force_fallback: DEFAULT_UNLOCK_FORCE_FALLBACK,
            delete_force_fallback: DEFAULT_DELETE_FORCE_FALLBACK,
        }
    }
}

impl MenuTextConfig {
    pub fn dialog_title(&self) -> &str {
        &self.root_menu_text
    }
}

#[derive(Clone, Debug)]
pub struct HelperRequest {
    pub action: BridgeAction,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct HelperResult {
    pub reboot_required: bool,
    pub error_message: Option<String>,
    pub failed: Vec<FailedPathResult>,
}

#[derive(Clone, Debug)]
pub struct FailedPathResult {
    pub path: PathBuf,
    pub status: u32,
}

pub fn config_path_for_dir(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE_NAME)
}

pub fn helper_path_for_dir(dir: &Path) -> PathBuf {
    dir.join(HELPER_EXE_NAME)
}

pub fn worker_dll_path_for_dir(dir: &Path) -> PathBuf {
    dir.join(WORKER_DLL_NAME)
}

pub fn unlocker_exe_path_for_dir(dir: &Path) -> PathBuf {
    dir.join("IObitUnlocker.exe")
}

pub fn unlocker_dll_path_for_dir(dir: &Path) -> PathBuf {
    dir.join("IObitUnlocker.dll")
}

pub fn read_menu_text_config(dir: &Path) -> MenuTextConfig {
    let path = config_path_for_dir(dir);
    match fs::read_to_string(&path) {
        Ok(content) => parse_menu_text_config(&content),
        Err(error) => {
            log_event(&format!(
                "config: using defaults because {} could not be read: {error}",
                path.to_string_lossy()
            ));
            MenuTextConfig::default()
        }
    }
}

fn parse_menu_text_config(content: &str) -> MenuTextConfig {
    let mut config = MenuTextConfig::default();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }

        match key {
            "root_menu_text" => config.root_menu_text = value.to_string(),
            "unlock_menu_text" => config.unlock_menu_text = value.to_string(),
            "delete_menu_text" => config.delete_menu_text = value.to_string(),
            "debug_log" => config.debug_log = parse_bool_value(value),
            "unlock_force_fallback" => config.unlock_force_fallback = parse_bool_value(value),
            "delete_force_fallback" => config.delete_force_fallback = parse_bool_value(value),
            _ => {}
        }
    }

    config
}

fn parse_bool_value(value: &str) -> bool {
    matches!(
        value.trim(),
        "1" | "true" | "True" | "TRUE" | "yes" | "Yes" | "YES" | "on" | "On" | "ON"
    )
}

pub fn write_request_file(request: &HelperRequest) -> io::Result<PathBuf> {
    let temp_dir = env::temp_dir();
    fs::create_dir_all(&temp_dir)?;

    for _ in 0..16 {
        let request_path = temp_dir.join(unique_request_file_name());
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&request_path)
        {
            Ok(mut file) => {
                writeln!(file, "action={}", request.action.as_config_value())?;
                for path in &request.paths {
                    writeln!(file, "path={}", path.to_string_lossy())?;
                }
                return Ok(request_path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a unique helper request file",
    ))
}

pub fn read_request_file(path: &Path) -> io::Result<HelperRequest> {
    let file = OpenOptions::new().read(true).open(path)?;
    let reader = BufReader::new(file);

    let mut action = None;
    let mut paths = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        match key.trim() {
            "action" => action = BridgeAction::from_config_value(value),
            "path" => {
                let path = value.trim();
                if !path.is_empty() {
                    paths.push(PathBuf::from(path));
                }
            }
            _ => {}
        }
    }

    let Some(action) = action else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request file is missing the action field",
        ));
    };

    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request file does not contain any paths",
        ));
    }

    Ok(HelperRequest { action, paths })
}

pub fn result_path_for_request(request_path: &Path) -> PathBuf {
    let mut file_name = OsString::from(request_path.as_os_str());
    file_name.push(".result");
    PathBuf::from(file_name)
}

pub fn write_result_file(path: &Path, result: &HelperResult) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;

    writeln!(
        file,
        "reboot_required={}",
        if result.reboot_required { 1 } else { 0 }
    )?;

    if let Some(error_message) = &result.error_message {
        writeln!(file, "error={error_message}")?;
    }

    for failed in &result.failed {
        writeln!(
            file,
            "failed={}|{}",
            failed.status,
            failed.path.to_string_lossy()
        )?;
    }

    Ok(())
}

pub fn read_result_file(path: &Path) -> io::Result<HelperResult> {
    let file = OpenOptions::new().read(true).open(path)?;
    let reader = BufReader::new(file);
    let mut result = HelperResult::default();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        match key.trim() {
            "reboot_required" => {
                result.reboot_required =
                    matches!(value.trim(), "1" | "true" | "True" | "TRUE");
            }
            "error" => {
                let message = value.trim();
                if !message.is_empty() {
                    result.error_message = Some(message.to_string());
                }
            }
            "failed" => {
                let Some((status, path)) = value.split_once('|') else {
                    continue;
                };
                let Ok(status) = status.trim().parse::<u32>() else {
                    continue;
                };
                let path = path.trim();
                if path.is_empty() {
                    continue;
                }
                result.failed.push(FailedPathResult {
                    path: PathBuf::from(path),
                    status,
                });
            }
            _ => {}
        }
    }

    Ok(result)
}

fn unique_request_file_name() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);

    format!("iobitunlocker-shell-bridge-{pid}-{timestamp}-{counter}.req")
}

pub fn log_event(message: &str) {
    let log_path = env::temp_dir().join(LOG_FILE_NAME);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(file, "{message}");
    }
}

pub fn log_debug(enabled: bool, message: &str) {
    if enabled {
        log_event(message);
    }
}

pub fn quote_windows_argument(value: &str) -> String {
    if !value.is_empty()
        && !value.contains([' ', '\t', '\n', '\u{000B}', '"'])
        && !value.ends_with('\\')
    {
        return value.to_string();
    }

    let mut result = String::from("\"");
    let mut backslash_count = 0usize;

    for ch in value.chars() {
        match ch {
            '\\' => backslash_count += 1,
            '"' => {
                result.push_str(&"\\".repeat(backslash_count * 2 + 1));
                result.push('"');
                backslash_count = 0;
            }
            _ => {
                result.push_str(&"\\".repeat(backslash_count));
                backslash_count = 0;
                result.push(ch);
            }
        }
    }

    result.push_str(&"\\".repeat(backslash_count * 2));
    result.push('"');
    result
}

pub fn to_wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

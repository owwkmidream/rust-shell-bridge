#![cfg(target_os = "windows")]

#[path = "../../src/common.rs"]
mod common;

use std::{
    ffi::{c_void, CString},
    mem::transmute,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
};

use common::{
    log_debug, log_event, read_menu_text_config, read_request_file, result_path_for_request,
    to_wide_null, unlocker_dll_path_for_dir, write_result_file, BridgeAction, FailedPathResult,
    HelperRequest, HelperResult, MenuTextConfig,
};
use windows::Win32::{
    Foundation::{FreeLibrary, HMODULE},
    System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};
use windows_core::{PCSTR, PCWSTR};

type DriverStartFn = unsafe extern "system" fn() -> i32;
type DriverStopFn = unsafe extern "system" fn() -> i32;
type DriverUnlockFileFn = unsafe extern "system" fn(PCWSTR, PCWSTR, u32, u32, *mut u32) -> u32;

const DRIVER_OPTION_NORMAL: u32 = 0;
const DRIVER_OPTION_FORCE: u32 = 4;

#[no_mangle]
pub unsafe extern "system" fn RunBridgeRequestW(request_path: PCWSTR) -> u32 {
    match run_request(request_path) {
        Ok(()) => 0,
        Err(error) => {
            log_event(&format!("worker: fatal error: {error}"));
            if let Ok(request_path) = pcwstr_to_pathbuf(request_path) {
                let result_path = result_path_for_request(&request_path);
                let _ = write_result_file(
                    &result_path,
                    &HelperResult {
                        reboot_required: false,
                        error_message: Some(error.clone()),
                        failed: Vec::new(),
                    },
                );
            }
            1
        }
    }
}

fn run_request(request_path: PCWSTR) -> Result<(), String> {
    let request_path = pcwstr_to_pathbuf(request_path)?;
    let result_path = result_path_for_request(&request_path);
    let request = read_request_file(&request_path)
        .map_err(|error| format!("failed to read request file in worker: {error}"))?;

    let current_exe = std::env::current_exe()
        .map_err(|error| format!("worker failed to resolve current exe: {error}"))?;
    let deploy_dir = current_exe
        .parent()
        .ok_or_else(|| "worker current exe has no parent directory".to_string())?;
    let menu_config = read_menu_text_config(deploy_dir);
    let debug_log = menu_config.debug_log;

    log_debug(
        debug_log,
        &format!(
            "worker: action={:?}, count={}, request={}, result={}",
            request.action,
            request.paths.len(),
            request_path.to_string_lossy(),
            result_path.to_string_lossy()
        ),
    );

    let driver = DriverBridge::load(deploy_dir, &menu_config)?;
    let outcome = driver.execute(&request)?;
    write_result_file(&result_path, &outcome)
        .map_err(|error| format!("worker failed to write result file: {error}"))?;

    log_debug(
        debug_log,
        &format!(
            "worker: completed request with {} failed item(s), reboot_required={}",
            outcome.failed.len(),
            outcome.reboot_required
        ),
    );

    Ok(())
}

fn pcwstr_to_pathbuf(value: PCWSTR) -> Result<PathBuf, String> {
    if value.is_null() {
        return Err("worker received a null request path".to_string());
    }

    let mut len = 0usize;
    unsafe {
        while *value.0.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(value.0, len);
        let text = std::ffi::OsString::from_wide(slice);
        Ok(PathBuf::from(text))
    }
}

struct DriverBridge {
    module: HMODULE,
    driver_stop: DriverStopFn,
    driver_unlock_file: DriverUnlockFileFn,
    debug_log: bool,
    unlock_force_fallback: bool,
    delete_force_fallback: bool,
}

impl DriverBridge {
    fn load(deploy_dir: &Path, menu_config: &MenuTextConfig) -> Result<Self, String> {
        let debug_log = menu_config.debug_log;
        let dll_path = unlocker_dll_path_for_dir(deploy_dir);
        if !dll_path.is_file() {
            return Err(format!(
                "worker did not find IObitUnlocker.dll:\n{}",
                dll_path.to_string_lossy()
            ));
        }

        let dll_wide = to_wide_null(dll_path.to_string_lossy().as_ref());
        let module = unsafe { LoadLibraryW(PCWSTR(dll_wide.as_ptr())) }
            .map_err(|error| format!("worker LoadLibraryW failed for {}: {error}", dll_path.to_string_lossy()))?;

        let driver_start =
            unsafe { transmute::<*const c_void, DriverStartFn>(load_export_address(module, "DriverStart")?) };
        let driver_stop =
            unsafe { transmute::<*const c_void, DriverStopFn>(load_export_address(module, "DriverStop")?) };
        let driver_unlock_file = unsafe {
            transmute::<*const c_void, DriverUnlockFileFn>(load_export_address(
                module,
                "DriverUnlockFile",
            )?)
        };

        let start_ok = unsafe { driver_start() };
        if start_ok == 0 {
            unsafe {
                let _ = FreeLibrary(module);
            }
            return Err("worker DriverStart failed".to_string());
        }

        log_debug(debug_log, "worker: driver started successfully");

        Ok(Self {
            module,
            driver_stop,
            driver_unlock_file,
            debug_log,
            unlock_force_fallback: menu_config.unlock_force_fallback,
            delete_force_fallback: menu_config.delete_force_fallback,
        })
    }

    fn execute(&self, request: &HelperRequest) -> Result<HelperResult, String> {
        let mut outcome = HelperResult::default();

        for path in &request.paths {
            let result = self.execute_path(request.action, path);

            if result.reboot_required {
                outcome.reboot_required = true;
            }

            if result.status != 0 {
                outcome.failed.push(FailedPathResult {
                    path: path.clone(),
                    status: result.status,
                });
            }
        }

        Ok(outcome)
    }

    fn execute_path(&self, action: BridgeAction, path: &Path) -> PathActionResult {
        let normal = self.call_driver(path, action, DRIVER_OPTION_NORMAL);
        if normal.status == 0 || !self.force_fallback_enabled(action) {
            return normal;
        }

        let force = self.call_driver(path, action, DRIVER_OPTION_FORCE);
        PathActionResult {
            status: force.status,
            reboot_required: normal.reboot_required || force.reboot_required,
        }
    }

    fn force_fallback_enabled(&self, action: BridgeAction) -> bool {
        match action {
            BridgeAction::Unlock => self.unlock_force_fallback,
            BridgeAction::Delete => self.delete_force_fallback,
        }
    }

    fn call_driver(&self, path: &Path, action: BridgeAction, option_code: u32) -> PathActionResult {
        let src_wide = to_wide_null(path.to_string_lossy().as_ref());
        let empty_wide = to_wide_null("");
        let mut reboot_required = 0u32;
        let status = unsafe {
            (self.driver_unlock_file)(
                PCWSTR(src_wide.as_ptr()),
                PCWSTR(empty_wide.as_ptr()),
                action.operation_code(),
                option_code,
                &mut reboot_required,
            )
        };

        log_debug(
            self.debug_log,
            &format!(
                "worker: driver action={:?}, option={}, path={}, status={}, reboot_required={}",
                action,
                option_code,
                path.to_string_lossy(),
                status,
                reboot_required
            ),
        );

        PathActionResult {
            status,
            reboot_required: reboot_required != 0,
        }
    }
}

impl Drop for DriverBridge {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.driver_stop)();
            let _ = FreeLibrary(self.module);
        }
        log_debug(self.debug_log, "worker: driver stopped");
    }
}

#[derive(Clone, Copy)]
struct PathActionResult {
    status: u32,
    reboot_required: bool,
}

unsafe fn load_export_address(module: HMODULE, export_name: &str) -> Result<*const c_void, String> {
    let export_name = CString::new(export_name)
        .map_err(|error| format!("invalid export name {export_name:?}: {error}"))?;
    let address = GetProcAddress(module, PCSTR(export_name.as_ptr() as *const u8));
    let Some(address) = address else {
        return Err(format!("worker GetProcAddress failed for {}", export_name.to_string_lossy()));
    };

    Ok(address as *const c_void)
}

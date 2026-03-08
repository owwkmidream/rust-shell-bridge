#![cfg(target_os = "windows")]
#![windows_subsystem = "windows"]

mod common;

use std::{
    ffi::{c_void, CString},
    mem::{size_of, transmute},
    path::{Path, PathBuf},
    thread::sleep,
    time::Duration,
};

use common::{
    log_debug, log_event, quote_windows_argument, read_menu_text_config, read_request_file,
    read_result_file, result_path_for_request, to_wide_null, unlocker_exe_path_for_dir,
    worker_dll_path_for_dir, BridgeAction, HelperResult,
};
use windows::Win32::{
    Foundation::{
        CloseHandle, FreeLibrary, HANDLE, HMODULE, HWND, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::Debug::WriteProcessMemory,
        LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW},
        Memory::{VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE},
        Threading::{
            CreateProcessW, CreateRemoteThread, GetExitCodeThread, TerminateProcess, WaitForInputIdle,
            WaitForSingleObject, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTF_USESHOWWINDOW,
            STARTUPINFOW,
        },
    },
    UI::WindowsAndMessaging::{
        MessageBoxW, MESSAGEBOX_STYLE, MB_ICONERROR, MB_OK, SW_HIDE,
    },
};
use windows_core::{Error as WinError, PCSTR, PCWSTR, PWSTR};

const REMOTE_LOAD_LIBRARY_TIMEOUT_MS: u32 = 30_000;
const REMOTE_WORKER_BASE_TIMEOUT_MS: u32 = 30_000;
const REMOTE_WORKER_TIMEOUT_PER_ITEM_MS: u32 = 15_000;
const REMOTE_WORKER_TIMEOUT_MAX_MS: u32 = 15 * 60 * 1000;
const INPUT_IDLE_TIMEOUT_MS: u32 = 5_000;
const PROCESS_EXIT_WAIT_MS: u32 = 2_000;
const HOST_START_SETTLE: Duration = Duration::from_millis(500);
const WORKER_EXPORT_NAME: &str = "RunBridgeRequestW";

fn main() {
    if let Err(error) = run() {
        log_event(&format!("helper: fatal error: {error}"));
        show_error_dialog(dialog_title(), error);
    }
}

fn run() -> Result<(), String> {
    let runtime_args = parse_runtime_args()?;
    let helper_exe = std::env::current_exe()
        .map_err(|error| format!("failed to locate helper executable: {error}"))?;
    let helper_dir = helper_exe
        .parent()
        .ok_or_else(|| "helper executable has no parent directory".to_string())?;
    let deploy_dir = runtime_args.deploy_dir.as_deref().unwrap_or(helper_dir);
    let menu_config = read_menu_text_config(deploy_dir);
    let debug_log = menu_config.debug_log;
    let result_path = result_path_for_request(&runtime_args.request_path);
    let _cleanup = TempFileCleanup::new(vec![
        runtime_args.request_path.clone(),
        result_path.clone(),
    ]);
    let request = read_request_file(&runtime_args.request_path)
        .map_err(|error| format!("failed to read helper request file: {error}"))?;

    log_debug(
        debug_log,
        &format!(
            "helper: action={:?}, count={}, request={}, result={}",
            request.action,
            request.paths.len(),
            runtime_args.request_path.to_string_lossy(),
            result_path.to_string_lossy()
        ),
    );

    let _ = std::fs::remove_file(&result_path);

    run_request_via_official_host(
        deploy_dir,
        &runtime_args.request_path,
        request.paths.len(),
        debug_log,
    )?;

    let outcome = read_result_file(&result_path).map_err(|error| {
        format!(
            "worker 未返回可读结果文件：\n{}\n{}",
            result_path.to_string_lossy(),
            error
        )
    })?;

    if let Some(error_message) = &outcome.error_message {
        return Err(error_message.clone());
    }

    if !outcome.failed.is_empty() {
        show_error_dialog(
            dialog_title(),
            format_failure_summary(request.action, &outcome),
        );
    } else if outcome.reboot_required {
        log_debug(debug_log, "helper: request completed with reboot_required=true");
    }

    Ok(())
}

fn run_request_via_official_host(
    deploy_dir: &Path,
    request_path: &Path,
    path_count: usize,
    debug_log: bool,
) -> Result<(), String> {
    let unlocker_exe = unlocker_exe_path_for_dir(deploy_dir);
    if !unlocker_exe.is_file() {
        return Err(format!(
            "未找到官方宿主 IObitUnlocker.exe：\n{}",
            unlocker_exe.to_string_lossy()
        ));
    }

    let worker_dll = worker_dll_path_for_dir(deploy_dir);
    if !worker_dll.is_file() {
        return Err(format!(
            "未找到 worker DLL：\n{}",
            worker_dll.to_string_lossy()
        ));
    }

    let mut host = HiddenHostProcess::spawn(&unlocker_exe, deploy_dir, debug_log)?;
    host.inject_and_run_worker(&worker_dll, request_path, path_count)?;
    Ok(())
}

fn parse_runtime_args() -> Result<RuntimeArgs, String> {
    let mut args = std::env::args_os().skip(1);
    let mut request_path = None;
    let mut deploy_dir = None;

    while let Some(arg) = args.next() {
        if arg == "--request" {
            let Some(value) = args.next() else {
                return Err("missing value after --request".to_string());
            };
            request_path = Some(PathBuf::from(value));
            continue;
        }

        if arg == "--deploy-dir" {
            let Some(value) = args.next() else {
                return Err("missing value after --deploy-dir".to_string());
            };
            deploy_dir = Some(PathBuf::from(value));
            continue;
        }
    }

    let Some(request_path) = request_path else {
        return Err("helper expects --request <file>".to_string());
    };

    Ok(RuntimeArgs {
        request_path,
        deploy_dir,
    })
}

fn dialog_title() -> String {
    let deploy_dir = parse_runtime_args()
        .ok()
        .and_then(|args| args.deploy_dir)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        });

    deploy_dir
        .as_deref()
        .map(read_menu_text_config)
        .unwrap_or_default()
        .dialog_title()
        .to_string()
}

struct RuntimeArgs {
    request_path: PathBuf,
    deploy_dir: Option<PathBuf>,
}

struct HiddenHostProcess {
    process: HANDLE,
    thread: HANDLE,
    terminated: bool,
    debug_log: bool,
}

impl HiddenHostProcess {
    fn spawn(exe_path: &Path, working_dir: &Path, debug_log: bool) -> Result<Self, String> {
        let exe_wide = to_wide_null(exe_path.to_string_lossy().as_ref());
        let mut command_line =
            to_wide_null(&quote_windows_argument(exe_path.to_string_lossy().as_ref()));
        let working_dir_wide = to_wide_null(working_dir.to_string_lossy().as_ref());

        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESHOWWINDOW,
            wShowWindow: SW_HIDE.0 as u16,
            ..Default::default()
        };
        let mut process_info = PROCESS_INFORMATION::default();

        unsafe {
            CreateProcessW(
                PCWSTR(exe_wide.as_ptr()),
                PWSTR(command_line.as_mut_ptr()),
                None,
                None,
                false,
                PROCESS_CREATION_FLAGS(0),
                None,
                PCWSTR(working_dir_wide.as_ptr()),
                &startup,
                &mut process_info,
            )
            .map_err(|error| format!("failed to launch official host process: {error}"))?;
        }

        log_debug(
            debug_log,
            &format!(
                "helper: launched official host pid={} exe={}",
                process_info.dwProcessId,
                exe_path.to_string_lossy()
            ),
        );

        let wait_result = unsafe { WaitForInputIdle(process_info.hProcess, INPUT_IDLE_TIMEOUT_MS) };
        log_debug(
            debug_log,
            &format!("helper: WaitForInputIdle returned {}", wait_result),
        );
        sleep(HOST_START_SETTLE);

        Ok(Self {
            process: process_info.hProcess,
            thread: process_info.hThread,
            terminated: false,
            debug_log,
        })
    }

    fn inject_and_run_worker(
        &mut self,
        worker_dll: &Path,
        request_path: &Path,
        path_count: usize,
    ) -> Result<(), String> {
        let remote_module = inject_library(self.process, worker_dll, self.debug_log)?;
        let remote_request = RemoteAllocation::write_wide_string(
            self.process,
            request_path.to_string_lossy().as_ref(),
        )?;
        let local_worker = LocalLibrary::load(worker_dll)?;
        let export = unsafe { load_export_address(local_worker.module, WORKER_EXPORT_NAME)? };
        let export_offset = export as usize - local_worker.module.0 as usize;
        let remote_export = (remote_module + export_offset) as *const c_void;

        log_debug(
            self.debug_log,
            &format!(
                "helper: worker injected module_base=0x{:08X}, export_offset=0x{:08X}",
                remote_module,
                export_offset
            ),
        );

        let timeout_ms = worker_timeout_ms(path_count);
        let exit_code = start_remote_thread(
            self.process,
            remote_export,
            remote_request.address(),
            timeout_ms,
        )?;
        log_debug(
            self.debug_log,
            &format!("helper: worker thread exited with {}", exit_code),
        );

        if exit_code != 0 {
            log_debug(
                self.debug_log,
                &format!("helper: worker thread returned non-zero code {}", exit_code),
            );
        }

        Ok(())
    }
}

impl Drop for HiddenHostProcess {
    fn drop(&mut self) {
        unsafe {
            if !self.terminated {
                let _ = TerminateProcess(self.process, 0);
                let _ = WaitForSingleObject(self.process, PROCESS_EXIT_WAIT_MS);
                self.terminated = true;
                log_debug(self.debug_log, "helper: official host process terminated");
            }
            if !self.thread.is_invalid() {
                let _ = CloseHandle(self.thread);
            }
            if !self.process.is_invalid() {
                let _ = CloseHandle(self.process);
            }
        }
    }
}

struct LocalLibrary {
    module: HMODULE,
}

impl LocalLibrary {
    fn load(path: &Path) -> Result<Self, String> {
        let path_wide = to_wide_null(path.to_string_lossy().as_ref());
        let module = unsafe { LoadLibraryW(PCWSTR(path_wide.as_ptr())) }
            .map_err(|error| format!("failed to load local library {}: {error}", path.to_string_lossy()))?;
        Ok(Self { module })
    }
}

impl Drop for LocalLibrary {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeLibrary(self.module);
        }
    }
}

struct RemoteAllocation {
    process: HANDLE,
    base: *mut c_void,
}

impl RemoteAllocation {
    fn write_wide_string(process: HANDLE, value: &str) -> Result<Self, String> {
        let buffer = to_wide_null(value);
        let byte_len = buffer.len() * size_of::<u16>();
        let base = unsafe {
            VirtualAllocEx(
                process,
                None,
                byte_len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };

        if base.is_null() {
            return Err(format!("VirtualAllocEx failed: {}", WinError::from_win32()));
        }

        let mut written = 0usize;
        let write_ok = unsafe {
            WriteProcessMemory(
                process,
                base,
                buffer.as_ptr() as *const c_void,
                byte_len,
                Some(&mut written),
            )
        };

        if let Err(error) = write_ok {
            unsafe {
                let _ = VirtualFreeEx(process, base, 0, MEM_RELEASE);
            }
            return Err(format!("WriteProcessMemory failed: {error}"));
        }

        if written != byte_len {
            unsafe {
                let _ = VirtualFreeEx(process, base, 0, MEM_RELEASE);
            }
            return Err(format!(
                "WriteProcessMemory wrote {} of {} bytes",
                written, byte_len
            ));
        }

        Ok(Self { process, base })
    }

    fn address(&self) -> *mut c_void {
        self.base
    }
}

impl Drop for RemoteAllocation {
    fn drop(&mut self) {
        unsafe {
            if !self.base.is_null() {
                let _ = VirtualFreeEx(self.process, self.base, 0, MEM_RELEASE);
            }
        }
    }
}

fn inject_library(process: HANDLE, dll_path: &Path, debug_log: bool) -> Result<usize, String> {
    let remote_dll_path = RemoteAllocation::write_wide_string(process, dll_path.to_string_lossy().as_ref())?;
    let kernel32 = unsafe { GetModuleHandleW(windows_core::w!("kernel32.dll")) }
        .map_err(|error| format!("GetModuleHandleW(kernel32.dll) failed: {error}"))?;
    let load_library = unsafe {
        load_export_address(kernel32, "LoadLibraryW")?
    };

    let remote_module = start_remote_thread(
        process,
        load_library,
        remote_dll_path.address(),
        REMOTE_LOAD_LIBRARY_TIMEOUT_MS,
    )?;
    if remote_module == 0 {
        return Err("remote LoadLibraryW returned null module handle".to_string());
    }

    log_debug(
        debug_log,
        &format!(
            "helper: remote LoadLibraryW loaded {} at 0x{:08X}",
            dll_path.to_string_lossy(),
            remote_module
        ),
    );

    Ok(remote_module as usize)
}

fn start_remote_thread(
    process: HANDLE,
    entry: *const c_void,
    parameter: *mut c_void,
    timeout_ms: u32,
) -> Result<u32, String> {
    let entry = unsafe {
        transmute::<*const c_void, unsafe extern "system" fn(*mut c_void) -> u32>(entry)
    };

    let thread = unsafe {
        CreateRemoteThread(process, None, 0, Some(entry), Some(parameter), 0, None)
            .map_err(|error| format!("CreateRemoteThread failed: {error}"))?
    };

    let wait = unsafe { WaitForSingleObject(thread, timeout_ms) };
    if wait != WAIT_OBJECT_0 {
        unsafe {
            let _ = CloseHandle(thread);
        }
        if wait == WAIT_TIMEOUT {
            return Err(format!("remote thread timed out after {} ms", timeout_ms));
        }
        if wait == WAIT_FAILED {
            return Err(format!(
                "remote thread wait failed: {}",
                WinError::from_win32()
            ));
        }
        return Err(format!("remote thread wait failed with code {}", wait.0));
    }

    let mut exit_code = 0u32;
    unsafe {
        GetExitCodeThread(thread, &mut exit_code)
            .map_err(|error| format!("GetExitCodeThread failed: {error}"))?;
        let _ = CloseHandle(thread);
    }

    Ok(exit_code)
}

fn worker_timeout_ms(path_count: usize) -> u32 {
    let extra = path_count.saturating_mul(REMOTE_WORKER_TIMEOUT_PER_ITEM_MS as usize);
    let timeout = REMOTE_WORKER_BASE_TIMEOUT_MS.saturating_add(extra.min(u32::MAX as usize) as u32);
    timeout.min(REMOTE_WORKER_TIMEOUT_MAX_MS)
}

unsafe fn load_export_address(module: HMODULE, export_name: &str) -> Result<*const c_void, String> {
    let export_name = CString::new(export_name)
        .map_err(|error| format!("invalid export name {export_name:?}: {error}"))?;
    let address = GetProcAddress(module, PCSTR(export_name.as_ptr() as *const u8));
    let Some(address) = address else {
        return Err(format!("GetProcAddress failed for {}", export_name.to_string_lossy()));
    };

    Ok(address as *const c_void)
}

fn format_failure_summary(action: BridgeAction, outcome: &HelperResult) -> String {
    let verb = match action {
        BridgeAction::Unlock => "解锁",
        BridgeAction::Delete => "解锁并删除",
    };

    let mut message = format!("{verb}失败，共 {} 个项目失败。\n", outcome.failed.len());
    for failed in outcome.failed.iter().take(8) {
        message.push_str(&format!(
            "\n- [{}] {}",
            failed.status,
            failed.path.to_string_lossy()
        ));
    }

    if outcome.failed.len() > 8 {
        message.push_str(&format!(
            "\n\n其余 {} 个失败项目已省略。",
            outcome.failed.len() - 8
        ));
    }

    if outcome.reboot_required {
        message.push_str("\n\n另有部分项目被标记为需要重启后完成。");
    }

    message
}

struct TempFileCleanup {
    paths: Vec<PathBuf>,
}

impl TempFileCleanup {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn show_error_dialog(title: String, message: String) {
    show_dialog(&title, &message, MB_OK | MB_ICONERROR);
}

fn show_dialog(title: &str, message: &str, flags: MESSAGEBOX_STYLE) {
    let title_wide = to_wide_null(title);
    let message_wide = to_wide_null(message);

    unsafe {
        let _ = MessageBoxW(
            HWND::default(),
            PCWSTR(message_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            flags,
        );
    }
}

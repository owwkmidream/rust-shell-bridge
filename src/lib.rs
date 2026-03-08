#![cfg(target_os = "windows")]

mod common;

use std::{
    ffi::c_void,
    mem::size_of,
    path::{Path, PathBuf},
    ptr::null_mut,
    sync::{
        atomic::{AtomicIsize, AtomicU32, Ordering},
        Mutex,
    },
};

use common::{
    helper_path_for_dir, log_debug, log_event, quote_windows_argument, read_menu_text_config,
    to_wide_null, unlocker_exe_path_for_dir, BridgeAction, HelperRequest,
};
use windows::Win32::{
    Foundation::{
        BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_NOTIMPL,
        HINSTANCE, HMODULE, HWND, RECT, S_FALSE, S_OK,
    },
    Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, FillRect, GetDC,
        GetSysColorBrush, ReleaseDC, SelectObject, COLOR_MENU, HBITMAP, HBRUSH,
    },
    System::{
        Com::{
            IClassFactory, IClassFactory_Impl, IDataObject, DVASPECT_CONTENT, FORMATETC, STGMEDIUM,
            TYMED_HGLOBAL,
        },
        LibraryLoader::GetModuleFileNameW,
        Ole::{ReleaseStgMedium, CF_HDROP},
        Registry::HKEY,
        SystemServices::DLL_PROCESS_ATTACH,
    },
    UI::{
        Shell::{
            Common::ITEMIDLIST, DragQueryFileW, ExtractIconExW, IContextMenu, IContextMenu_Impl,
            IShellExtInit, IShellExtInit_Impl, ShellExecuteW, CMF_DEFAULTONLY, CMINVOKECOMMANDINFO,
            HDROP, SE_ERR_ACCESSDENIED,
        },
        WindowsAndMessaging::{
            CreatePopupMenu, DestroyIcon, DestroyMenu, DrawIconEx, GetSystemMetrics, InsertMenuW,
            MessageBoxW, SetMenuItemInfoW, DI_NORMAL, HICON, HMENU, MB_ICONERROR, MB_OK,
            MENUITEMINFOW, MF_BYPOSITION, MF_POPUP, MF_STRING, MIIM_BITMAP, SM_CXMENUCHECK,
            SM_CYMENUCHECK, SW_SHOWNORMAL,
        },
    },
};
use windows_core::{implement, Error, IUnknown, Interface, Result, GUID, HRESULT, PCWSTR, PSTR};

const CLSID_UNLOCKER_SHELL_EXT: GUID = GUID::from_u128(0x8e61a8fd_0b37_4aeb_9ce0_9d833295673f);
const MENU_COMMAND_COUNT: i32 = 2;

static DLL_MODULE: AtomicIsize = AtomicIsize::new(0);
static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);
static SERVER_LOCKS: AtomicU32 = AtomicU32::new(0);

struct DllRefGuard;

impl DllRefGuard {
    fn new() -> Self {
        LIVE_OBJECTS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for DllRefGuard {
    fn drop(&mut self) {
        LIVE_OBJECTS.fetch_sub(1, Ordering::Relaxed);
    }
}

#[implement(IShellExtInit, IContextMenu)]
struct UnlockerShellExt {
    _guard: DllRefGuard,
    menu_bitmap: Mutex<Option<HBITMAP>>,
    selected_paths: Mutex<Vec<PathBuf>>,
}

impl UnlockerShellExt {
    fn new() -> Self {
        Self {
            _guard: DllRefGuard::new(),
            menu_bitmap: Mutex::new(None),
            selected_paths: Mutex::new(Vec::new()),
        }
    }

    fn replace_selection(&self, paths: Vec<PathBuf>) -> Result<()> {
        let mut guard = self.selected_paths.lock().map_err(|_| lock_error())?;
        *guard = paths;
        Ok(())
    }

    fn current_selection(&self) -> Result<Vec<PathBuf>> {
        let guard = self.selected_paths.lock().map_err(|_| lock_error())?;
        Ok(guard.clone())
    }

    fn ensure_menu_bitmap(&self, unlocker_path: &Path) -> Result<HBITMAP> {
        let mut guard = self.menu_bitmap.lock().map_err(|_| lock_error())?;
        if let Some(bitmap) = *guard {
            return Ok(bitmap);
        }

        let bitmap = unsafe { create_menu_bitmap(unlocker_path)? };
        *guard = Some(bitmap);
        Ok(bitmap)
    }
}

impl Drop for UnlockerShellExt {
    fn drop(&mut self) {
        if let Ok(slot) = self.menu_bitmap.get_mut() {
            if let Some(bitmap) = slot.take() {
                if !bitmap.0.is_null() {
                    unsafe {
                        let _ = DeleteObject(bitmap);
                    }
                }
            }
        }
    }
}

impl IShellExtInit_Impl for UnlockerShellExt_Impl {
    fn Initialize(
        &self,
        _pidlfolder: *const ITEMIDLIST,
        pdtobj: Option<&IDataObject>,
        _hkeyprogid: HKEY,
    ) -> Result<()> {
        let paths = match pdtobj {
            Some(data_object) => unsafe { extract_paths_from_data_object(data_object) }
                .unwrap_or_default(),
            None => Vec::new(),
        };

        self.replace_selection(paths)
    }
}

impl IContextMenu_Impl for UnlockerShellExt_Impl {
    fn QueryContextMenu(
        &self,
        hmenu: HMENU,
        indexmenu: u32,
        idcmdfirst: u32,
        _idcmdlast: u32,
        uflags: u32,
    ) -> Result<()> {
        if (uflags & CMF_DEFAULTONLY) != 0 {
            return Ok(());
        }

        let selection = self.current_selection()?;
        if selection.is_empty() {
            return Ok(());
        }

        let dll_path = current_module_path()?;
        let deploy_dir = dll_path
            .parent()
            .ok_or_else(|| Error::new(E_FAIL, "shell extension DLL has no parent directory"))?;
        let menu_config = read_menu_text_config(deploy_dir);
        let debug_log = menu_config.debug_log;
        let helper_path = helper_path_for_dir(deploy_dir);
        let unlocker_path = unlocker_exe_path_for_dir(deploy_dir);

        if !helper_path.is_file() {
            log_event(&format!(
                "QueryContextMenu: skipped because helper was not found: {}",
                helper_path.to_string_lossy()
            ));
            return Ok(());
        }

        if !unlocker_path.is_file() {
            log_event(&format!(
                "QueryContextMenu: skipped because IObitUnlocker.exe was not found: {}",
                unlocker_path.to_string_lossy()
            ));
            return Ok(());
        }

        let root_text_wide = to_wide_null(&menu_config.root_menu_text);
        let unlock_text_wide = to_wide_null(&menu_config.unlock_menu_text);
        let delete_text_wide = to_wide_null(&menu_config.delete_menu_text);

        let submenu = unsafe { CreatePopupMenu()? };
        let insert_result = unsafe {
            InsertMenuW(
                submenu,
                0,
                MF_BYPOSITION | MF_STRING,
                idcmdfirst as usize,
                PCWSTR(unlock_text_wide.as_ptr()),
            )
            .and_then(|_| {
                InsertMenuW(
                    submenu,
                    1,
                    MF_BYPOSITION | MF_STRING,
                    (idcmdfirst + 1) as usize,
                    PCWSTR(delete_text_wide.as_ptr()),
                )
            })
            .and_then(|_| {
                InsertMenuW(
                    hmenu,
                    indexmenu,
                    MF_BYPOSITION | MF_POPUP,
                    submenu.0 as usize,
                    PCWSTR(root_text_wide.as_ptr()),
                )
            })
        };

        if let Err(error) = insert_result {
            unsafe {
                let _ = DestroyMenu(submenu);
            }
            log_event(&format!("QueryContextMenu: InsertMenuW failed: {error}"));
            return Err(error);
        }

        match self.ensure_menu_bitmap(&unlocker_path) {
            Ok(bitmap) => {
                let menu_item_info = MENUITEMINFOW {
                    cbSize: size_of::<MENUITEMINFOW>() as u32,
                    fMask: MIIM_BITMAP,
                    hbmpItem: bitmap,
                    ..Default::default()
                };

                if let Err(error) =
                    unsafe { SetMenuItemInfoW(hmenu, indexmenu, true, &menu_item_info) }
                {
                    log_event(&format!(
                        "QueryContextMenu: SetMenuItemInfoW failed: {error}"
                    ));
                }
            }
            Err(error) => {
                log_event(&format!(
                    "QueryContextMenu: menu icon creation failed: {error}"
                ));
            }
        }

        log_debug(debug_log, "QueryContextMenu: menu inserted successfully");
        Err(HRESULT(MENU_COMMAND_COUNT).into())
    }

    fn InvokeCommand(&self, pici: *const CMINVOKECOMMANDINFO) -> Result<()> {
        if pici.is_null() {
            return Err(E_INVALIDARG.into());
        }

        let info = unsafe { &*pici };
        let command_id = command_id_from_invoke_info(info)?;
        let Some(action) = BridgeAction::from_command_id(command_id) else {
            return Err(E_INVALIDARG.into());
        };

        let selection = self.current_selection()?;
        if selection.is_empty() {
            return Err(E_FAIL.into());
        }

        if let Ok(dll_path) = current_module_path() {
            if let Some(deploy_dir) = dll_path.parent() {
                let debug_log = read_menu_text_config(deploy_dir).debug_log;
                log_debug(
                    debug_log,
                    &format!("InvokeCommand: action={:?}, count={}", action, selection.len()),
                );
            }
        }

        spawn_helper(action, &selection, info.hwnd)
    }

    fn GetCommandString(
        &self,
        _idcmd: usize,
        _utype: u32,
        _preserved: *const u32,
        _pszname: PSTR,
        _cchmax: u32,
    ) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory)]
struct ShellExtClassFactory {
    _guard: DllRefGuard,
}

impl ShellExtClassFactory {
    fn new() -> Self {
        Self {
            _guard: DllRefGuard::new(),
        }
    }
}

impl IClassFactory_Impl for ShellExtClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }

        if riid.is_null() || ppvobject.is_null() {
            return Err(E_INVALIDARG.into());
        }

        let unknown: IUnknown = UnlockerShellExt::new().into();
        unsafe { unknown.query(riid, ppvobject).ok()? };
        Ok(())
    }

    fn LockServer(&self, flock: BOOL) -> Result<()> {
        if flock.as_bool() {
            SERVER_LOCKS.fetch_add(1, Ordering::Relaxed);
        } else {
            SERVER_LOCKS.fetch_sub(1, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn lock_error() -> Error {
    Error::new(E_FAIL, "internal synchronization failure")
}

unsafe fn extract_paths_from_data_object(data_object: &IDataObject) -> Result<Vec<PathBuf>> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };

    let mut medium: STGMEDIUM = data_object.GetData(&format)?;
    let hdrop = HDROP(medium.u.hGlobal.0);
    let file_count = DragQueryFileW(hdrop, u32::MAX, None);
    let mut paths = Vec::with_capacity(file_count as usize);

    for index in 0..file_count {
        let required_len = DragQueryFileW(hdrop, index, None);
        if required_len == 0 {
            continue;
        }

        let mut buffer = vec![0u16; required_len as usize + 1];
        let written_len = DragQueryFileW(hdrop, index, Some(buffer.as_mut_slice()));
        if written_len == 0 {
            continue;
        }

        let value = String::from_utf16_lossy(&buffer[..written_len as usize]);
        if !value.is_empty() {
            paths.push(PathBuf::from(value));
        }
    }

    ReleaseStgMedium(&mut medium);
    Ok(paths)
}

fn command_id_from_invoke_info(info: &CMINVOKECOMMANDINFO) -> Result<usize> {
    let raw = info.lpVerb.0 as usize;
    if (raw >> 16) != 0 {
        return Err(E_INVALIDARG.into());
    }

    Ok(raw & 0xffff)
}

fn spawn_helper(action: BridgeAction, selection: &[PathBuf], owner_hwnd: HWND) -> Result<()> {
    let dll_path = current_module_path()?;
    let deploy_dir = dll_path
        .parent()
        .ok_or_else(|| Error::new(E_FAIL, "shell extension DLL has no parent directory"))?;
    let menu_config = read_menu_text_config(deploy_dir);
    let debug_log = menu_config.debug_log;
    let helper_path = helper_path_for_dir(deploy_dir);

    if !helper_path.is_file() {
        let message = format!(
            "未找到 helper 可执行文件：\n{}",
            helper_path.to_string_lossy()
        );
        show_error_dialog(owner_hwnd, menu_config.dialog_title(), &message);
        return Err(Error::new(E_FAIL, message));
    }

    let request_path = common::write_request_file(&HelperRequest {
        action,
        paths: selection.to_vec(),
    })
    .map_err(|error| Error::new(E_FAIL, format!("failed to create helper request file: {error}")))?;

    let file_wide = to_wide_null(helper_path.to_string_lossy().as_ref());
    let directory_wide = to_wide_null(deploy_dir.to_string_lossy().as_ref());
    let parameters = format!(
        "--request {}",
        quote_windows_argument(request_path.to_string_lossy().as_ref())
    );
    let parameters_wide = to_wide_null(&parameters);

    log_debug(
        debug_log,
        &format!(
            "spawn_helper: helper={}, request={}",
            helper_path.to_string_lossy(),
            request_path.to_string_lossy()
        ),
    );

    let result = unsafe {
        ShellExecuteW(
            owner_hwnd,
            windows_core::w!("runas"),
            PCWSTR(file_wide.as_ptr()),
            PCWSTR(parameters_wide.as_ptr()),
            PCWSTR(directory_wide.as_ptr()),
            SW_SHOWNORMAL,
        )
    };

    let status = result.0 as isize;
    if status <= 32 {
        let _ = std::fs::remove_file(&request_path);
        let detail = if status as u32 == SE_ERR_ACCESSDENIED {
            "权限被拒绝，或 UAC 提权已取消"
        } else {
            "ShellExecuteW 返回了 shell 错误"
        };
        let message = format!("启动提权 helper 失败：{detail} (code {status})");
        show_error_dialog(owner_hwnd, menu_config.dialog_title(), &message);
        return Err(Error::new(E_FAIL, message));
    }

    Ok(())
}

fn show_error_dialog(owner_hwnd: HWND, title: &str, message: &str) {
    let title_wide = to_wide_null(title);
    let message_wide = to_wide_null(message);

    unsafe {
        let _ = MessageBoxW(
            owner_hwnd,
            PCWSTR(message_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

unsafe fn create_menu_bitmap(unlocker_path: &Path) -> Result<HBITMAP> {
    let path_wide = to_wide_null(unlocker_path.to_string_lossy().as_ref());
    let mut small_icon = HICON::default();
    let extracted = ExtractIconExW(
        PCWSTR(path_wide.as_ptr()),
        0,
        None,
        Some(&mut small_icon),
        1,
    );

    if extracted == 0 || small_icon.0.is_null() {
        return Err(Error::new(
            E_FAIL,
            format!(
                "failed to extract icon from {}",
                unlocker_path.to_string_lossy()
            ),
        ));
    }

    let width = GetSystemMetrics(SM_CXMENUCHECK).max(16);
    let height = GetSystemMetrics(SM_CYMENUCHECK).max(16);
    let screen_dc = GetDC(HWND::default());
    if screen_dc.0.is_null() {
        let _ = DestroyIcon(small_icon);
        return Err(Error::from_win32());
    }

    let memory_dc = CreateCompatibleDC(screen_dc);
    if memory_dc.0.is_null() {
        let _ = ReleaseDC(HWND::default(), screen_dc);
        let _ = DestroyIcon(small_icon);
        return Err(Error::from_win32());
    }

    let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
    if bitmap.0.is_null() {
        let _ = DeleteDC(memory_dc);
        let _ = ReleaseDC(HWND::default(), screen_dc);
        let _ = DestroyIcon(small_icon);
        return Err(Error::from_win32());
    }

    let previous = SelectObject(memory_dc, bitmap);
    let rect = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: height,
    };
    let background_brush = GetSysColorBrush(COLOR_MENU);
    let _ = FillRect(memory_dc, &rect, background_brush);
    let draw_result = DrawIconEx(
        memory_dc,
        0,
        0,
        small_icon,
        width,
        height,
        0,
        HBRUSH::default(),
        DI_NORMAL,
    );

    let _ = SelectObject(memory_dc, previous);
    let _ = DeleteDC(memory_dc);
    let _ = ReleaseDC(HWND::default(), screen_dc);
    let _ = DestroyIcon(small_icon);

    if let Err(error) = draw_result {
        let _ = DeleteObject(bitmap);
        return Err(error);
    }

    Ok(bitmap)
}

fn current_module_path() -> Result<PathBuf> {
    let module = HMODULE(DLL_MODULE.load(Ordering::Relaxed) as *mut c_void);
    if module.0.is_null() {
        return Err(E_FAIL.into());
    }

    let mut buffer = vec![0u16; 32768];
    let length = unsafe { GetModuleFileNameW(module, &mut buffer) } as usize;
    if length == 0 {
        return Err(Error::from_win32());
    }

    Ok(Path::new(&String::from_utf16_lossy(&buffer[..length])).to_path_buf())
}

#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE_OBJECTS.load(Ordering::Relaxed) == 0 && SERVER_LOCKS.load(Ordering::Relaxed) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_INVALIDARG;
    }

    *ppv = null_mut();

    if *rclsid != CLSID_UNLOCKER_SHELL_EXT {
        return CLASS_E_CLASSNOTAVAILABLE;
    }

    let factory: IUnknown = ShellExtClassFactory::new().into();
    factory.query(riid, ppv)
}

#[no_mangle]
pub extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        DLL_MODULE.store(hinst.0 as isize, Ordering::Relaxed);
    }

    BOOL(1)
}

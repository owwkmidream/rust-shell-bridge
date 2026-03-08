# 手动部署清单

这份清单对应 `rust-shell-bridge` 项目。

如果你只是想先理解整体设计、构建方式和运行原理，先看 [README.md](README.md)。

这个项目必须按“混合架构”部署：

- `iobitunlocker_shell_bridge.dll` 必须是 `x64`
- `iobitunlocker_shell_bridge_helper.exe` 必须是 `x86`

原因：

- 右键 Shell 扩展 DLL 要加载进 64 位 `explorer.exe`
- `IObitUnlocker.dll` / `IObitUnlocker.exe` 实测是 `x86 (32 位)`
- 所以真正调用 `IObitUnlocker.dll` 导出的 helper 也必须是 `x86`

如果你直接执行普通的 `cargo build --release`，通常会得到当前主机默认 target 的产物。在 64 位 Windows 上，这往往会生成 `x64 helper`，然后在 `LoadLibraryW("IObitUnlocker.dll")` 时触发：

```text
%1 不是有效的 Win32 应用程序。 (0x800700C1)
```

所以不要直接拿 `target\release\iobitunlocker_shell_bridge_helper.exe` 去部署。

## 1. 安装缺失的 Rust target

先确认已经安装这两个 target：

```powershell
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
```

## 2. 生成 deploy 目录

在项目目录执行：

```powershell
cd D:\Owwk_Software\IObitUnlocker\rust-shell-bridge
.\scripts\deploy.ps1
```

这个脚本会做三件事：

- 构建 `x64` shell extension DLL
- 构建 `x86` helper EXE
- 构建 `x86` worker DLL

然后把下面四个文件整理到 `deploy\`：

- `deploy\iobitunlocker_shell_bridge.dll`
- `deploy\iobitunlocker_shell_bridge_helper.exe`
- `deploy\iobitunlocker_shell_bridge_worker.dll`
- `deploy\iobitunlocker_shell_bridge.ini`

## 3. 准备最终同目录文件

最终部署目录里，下面 7 个文件必须同目录放置：

- `iobitunlocker_shell_bridge.dll`
- `iobitunlocker_shell_bridge_helper.exe`
- `iobitunlocker_shell_bridge_worker.dll`
- `iobitunlocker_shell_bridge.ini`
- `IObitUnlocker.exe`
- `IObitUnlocker.dll`
- `IObitUnlocker.sys`

建议直接把 `deploy\` 当成最终部署目录，然后手动再拷入 IObit 原文件。

原因：

- shell ext 会从 DLL 同目录查找 `helper.exe`
- shell 菜单图标会从同目录的 `IObitUnlocker.exe` 提取
- helper 会从同目录查找 `worker.dll`
- helper 会启动同目录官方 `IObitUnlocker.exe` 作为隐藏宿主进程
- `worker.dll` 会被注入到这个官方宿主进程里，再由官方宿主进程直调 `IObitUnlocker.dll` 导出
- `IObitUnlocker.dll` 又会继续依赖同目录的 `IObitUnlocker.sys`

## 4. 编辑菜单文本

打开同目录的 `iobitunlocker_shell_bridge.ini`，可改这三个键：

```ini
root_menu_text=Iobit Unlocker快捷操作
unlock_menu_text=解锁
delete_menu_text=删除
unlock_menu_hotkey=F
delete_menu_hotkey=D
debug_log=0
unlock_force_fallback=0
delete_force_fallback=0
```

说明：

- `root_menu_text`：右键一级菜单名称
- `unlock_menu_text`：子菜单“解锁”
- `delete_menu_text`：子菜单“删除”
- `unlock_menu_hotkey`：子菜单“解锁”的快捷键，默认 `F`
- `delete_menu_hotkey`：子菜单“删除”的快捷键，默认 `D`
- `debug_log`：是否写详细调试日志，`0` 为关闭，`1` 为开启
- `unlock_force_fallback`：解锁失败后是否自动回退到 `Force`，默认 `0`
- `delete_force_fallback`：删除失败后是否自动回退到 `Force`，默认 `0`

修改这个配置文件后，不需要重新编译。

如果你想关闭快捷键，可以把 `unlock_menu_hotkey` 或 `delete_menu_hotkey` 设为 `none`、`off`、`0` 或 `-`。

如果你想让注册表里 CLSID 的显示名同步更新，重新执行一次 `scripts\register.ps1` 即可。

## 5. 注册当前用户 Shell 扩展

最小注册命令：

```powershell
cd D:\Owwk_Software\IObitUnlocker\rust-shell-bridge
.\scripts\register.ps1
```

默认会注册 `deploy\iobitunlocker_shell_bridge.dll`。

如果你把 DLL 部署在别的目录，也可以显式指定：

```powershell
.\scripts\register.ps1 -DllPath "你的最终部署目录\iobitunlocker_shell_bridge.dll"
```

这个脚本会写入：

- `HKCU\Software\Classes\CLSID\{8E61A8FD-0B37-4AEB-9CE0-9D833295673F}`
- `HKCU\Software\Classes\AllFilesystemObjects\shellex\ContextMenuHandlers\IObitUnlockerShellBridge`
- `HKCU\Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved`

脚本本身不强制重启 `explorer.exe`。如果菜单没立刻刷新，手动重启资源管理器即可。

## 6. 验证

在资源管理器里选中一个或多个文件，检查：

- 能看到一级菜单
- 一级菜单图标复用 `IObitUnlocker.exe`
- 能看到两个子菜单
- 点击后只弹一次 UAC
- 成功默认不弹窗，失败才弹错误框
- 多选文件只走一次提权 helper
- 带逗号文件名也能正常处理

## 7. 如果你移动了部署目录

只要最终部署目录变了，就要重新执行注册脚本。

原因：

- 注册表里 `InprocServer32` 保存的是 DLL 绝对路径
- Shell 扩展运行时又按 DLL 所在目录查找 helper 和 IObit 文件

## 8. 卸载

如需卸载注册项：

```powershell
cd D:\Owwk_Software\IObitUnlocker\rust-shell-bridge
.\scripts\unregister.ps1
```

脚本只删除当前用户下的注册项，不会删除部署目录里的文件。

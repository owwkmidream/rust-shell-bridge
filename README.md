# IObit Unlocker Shell Bridge

一个面向 Windows 资源管理器的自定义右键扩展，用来替代 IObit Unlocker 自带 shell 扩展在多选场景下的糟糕体验。

当前只保留两个子菜单：

- `解锁`
- `解锁并删除`

设计目标很明确：

- 多选项目时只触发一次右键操作
- 整批项目只弹一次 UAC
- 成功默认静默
- 失败才弹错误框
- 右键菜单图标复用官方 `IObitUnlocker.exe`
- 菜单文本可通过同目录 `ini` 配置

## 解决了什么问题

官方 `IObitUnlocker.exe` 的命令行在多文件参数上行为不稳定。尤其是批量删除时，常见结果是只处理第一个文件，或者为了兼容各种文件名只能退回“每个文件单独调一次 CLI”，而这又会导致：

- 多次弹窗
- 多次 UAC
- 多选体验很差

这个项目不再依赖官方 CLI 批处理，而是直接走驱动导出调用链，同时把调用放在官方宿主进程里完成。

## 项目结构

```text
rust-shell-bridge/
├─ src/
│  ├─ lib.rs                         # x64 shell extension DLL
│  ├─ main.rs                        # x86 提权 helper EXE
│  └─ common.rs                      # 配置、请求/结果文件、日志、公用工具
├─ worker-dll/
│  ├─ Cargo.toml
│  └─ src/lib.rs                     # x86 worker DLL，注入官方宿主进程
├─ scripts/
│  ├─ deploy.ps1                     # 混合架构构建并整理 deploy 目录
│  ├─ register.ps1                   # 写 HKCU 注册表，注册右键扩展
│  └─ unregister.ps1                 # 删除 HKCU 注册项
├─ iobitunlocker_shell_bridge.ini    # 配置样例
└─ MANUAL_DEPLOY.md                  # 手动部署与排障清单
```

## 运行原理

最终工作链路如下：

```text
Explorer(x64)
  -> iobitunlocker_shell_bridge.dll (x64 shell ext)
  -> ShellExecuteW("runas") 启动 iobitunlocker_shell_bridge_helper.exe (x86)
  -> helper 隐藏启动官方 IObitUnlocker.exe (x86)
  -> helper 注入 iobitunlocker_shell_bridge_worker.dll (x86)
  -> worker 在官方宿主进程内 LoadLibraryW(IObitUnlocker.dll)
  -> 调用 DriverStart / DriverUnlockFile / DriverStop
```

更细一点的流程：

1. `explorer.exe` 加载 shell 扩展 DLL。
2. 右键菜单展示时，shell 扩展读取选中文件列表，并从同目录 `IObitUnlocker.exe` 提取菜单图标。
3. 用户点击“解锁”或“解锁并删除”后，shell 扩展把批量路径写到临时 request 文件。
4. shell 扩展用 `runas` 只启动一次 helper，因此整批操作只触发一次提权。
5. helper 隐藏启动同目录官方 `IObitUnlocker.exe`，然后把 worker DLL 注入进去。
6. worker 在官方宿主进程上下文内加载 `IObitUnlocker.dll`，直接调用其驱动相关导出。
7. worker 把结果写回 result 文件，helper 读取结果并决定是否弹错误框。

## 为什么必须使用“官方宿主进程注入”

这不是为了“炫技”，而是为了可靠性。

在这次分析里，已经验证过下面几种路线：

- 自定义 helper 直接 `LoadLibraryW(IObitUnlocker.dll)` 并调用导出：稳定返回 `87`
- 自定义 helper 改名伪装成 `IObitUnlocker.exe` 再调用：仍然返回 `87`
- 真正在官方 `IObitUnlocker.exe` 进程内调用同一组导出：返回 `0`，可以正常工作

这说明 `IObitUnlocker.dll` 不是“只要加载进任意 32 位进程就能用”的普通 DLL。它显然依赖官方 EXE 建立的某些运行时上下文、初始化状态、模块布局，或其他未公开约束。

所以这里的取舍是：

- 不继续浪费时间去赌官方 CLI 的多文件解析边界
- 不退回“每个文件调用一次 CLI”的多弹窗方案
- 直接让 worker 进入已验证可用的官方宿主环境

这个项目调用的是官方 DLL 导出的驱动接口，不走官方 CLI。

## 构建

### 目标架构

必须是混合架构：

- shell extension DLL：`x64`
- helper EXE：`x86`
- worker DLL：`x86`

原因：

- 资源管理器右键扩展要进 64 位 `explorer.exe`
- 官方 `IObitUnlocker.exe` / `IObitUnlocker.dll` 实测是 32 位
- 注入到官方宿主进程内的 worker 也必须是 32 位

### 安装 Rust target

```powershell
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc
```

### 生成 deploy 目录

```powershell
cd D:\Owwk_Software\IObitUnlocker\rust-shell-bridge
.\scripts\deploy.ps1
```

脚本会：

- 构建 `x64` shell extension DLL
- 构建 `x86` helper EXE
- 构建 `x86` worker DLL
- 把产物和配置样例整理到仓库内 `deploy\`

## 部署

最终部署目录里，下面这些文件必须同目录放置：

- `iobitunlocker_shell_bridge.dll`
- `iobitunlocker_shell_bridge_helper.exe`
- `iobitunlocker_shell_bridge_worker.dll`
- `iobitunlocker_shell_bridge.ini`
- `IObitUnlocker.exe`
- `IObitUnlocker.dll`
- `IObitUnlocker.sys`

推荐做法：

1. 先运行 `.\scripts\deploy.ps1`
2. 再把官方 3 个 IObit 文件手动拷进 `deploy\`
3. 最后用注册脚本把 `deploy\iobitunlocker_shell_bridge.dll` 注册到当前用户

更细的清单见 [MANUAL_DEPLOY.md](MANUAL_DEPLOY.md)。

## 注册方式

最小注册命令：

```powershell
cd D:\Owwk_Software\IObitUnlocker\rust-shell-bridge
.\scripts\register.ps1
```

默认注册仓库内 `deploy\iobitunlocker_shell_bridge.dll`。

如果你把 DLL 放在别的最终目录，可以显式传入：

```powershell
.\scripts\register.ps1 -DllPath "你的部署目录\iobitunlocker_shell_bridge.dll"
```

### 为什么不用 `regsvr32`

因为这个 DLL 没有实现 `DllRegisterServer` / `DllUnregisterServer` 这组自注册导出。

这里采用的是更直白的方式：

- 由 `register.ps1` 直接写 `HKCU\Software\Classes\CLSID\...`
- 写 `AllFilesystemObjects\shellex\ContextMenuHandlers\...`
- 写 `Shell Extensions\Approved`

也就是说，这个项目是 COM shell extension，但不是“自注册 DLL”。

## 配置

部署目录下的 `iobitunlocker_shell_bridge.ini` 支持以下键：

```ini
root_menu_text=Iobit Unlocker快捷操作
unlock_menu_text=解锁
delete_menu_text=解锁并删除
debug_log=0
```

说明：

- `root_menu_text`：一级菜单文本
- `unlock_menu_text`：子菜单“解锁”
- `delete_menu_text`：子菜单“解锁并删除”
- `debug_log`：是否输出详细调试日志，`0` 关闭，`1` 开启

只改配置文件即可，不需要重新编译。

## 日志与错误处理

默认行为：

- 成功不弹窗
- 失败才弹错误框
- 需要重启完成的情况默认也不额外提示

把 `debug_log=1` 后，会在系统临时目录写日志文件：

- `%TEMP%\iobitunlocker_shell_bridge.log`

日志主要用于定位：

- 菜单未显示
- helper 未找到
- worker 注入失败
- 官方宿主或官方 DLL 缺失
- 驱动调用返回错误码

## 当前限制

- 当前只实现“解锁”和“解锁并删除”
- 注册范围是当前用户 `HKCU`
- 当前挂在 `AllFilesystemObjects`
- 每次批量操作仍然需要一次 UAC，这是刻意保留的权限边界
- 如果官方 IObit 二进制版本变化很大，可能需要重新验证导出调用是否仍兼容

## 验证建议

建议手动验证以下场景：

- 单文件解锁
- 单文件删除
- 多文件批量删除
- 文件名包含空格
- 文件名包含逗号
- 成功路径是否默认静默
- 失败路径是否只弹一次错误框
- 菜单图标是否正确复用官方 EXE

## 许可与注意事项

这个仓库本身不包含官方 IObit 二进制的实现逻辑，只是桥接其现有安装文件。

提交仓库前建议不要把下面这些内容放进版本库：

- `deploy\`
- `target\`
- 任何官方 IObit 二进制

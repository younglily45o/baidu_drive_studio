# Baidu Drive Studio · Rust 百度网盘桌面客户端

面向 Windows 10/11 x64 的中文原生桌面应用，当前版本 **0.3.0**。Rust + egui/eframe，字体内嵌，无需安装 Rust 或 WebView2。支持完整 Cookies 登录和 OAuth 授权；本项目是第三方客户端。

0.3.0 将文件名列标题和内容改回左对齐，支持勾选文件夹并递归下载，保留子目录和空目录。沿用 0.2.2 的 Cookie 下载接口修复及下载阶段提示。

## 运行

解压 `dist/Baidu-Drive-Studio-Windows-x64.zip`，双击 `Baidu-Drive-Studio.exe`。界面使用 OpenGL，需要可用的显卡驱动。

1. 在浏览器登录 `https://pan.baidu.com`，按 F12 打开开发者工具，在“网络 / Network”中选择网盘的一个请求，从请求头复制完整 **Cookie** 值。
2. 在“登录与设置”的“Cookies 登录”中粘贴，点击“使用 Cookies 登录”。支持 `Cookie:` 前缀；0.2.1 自动忽略单独的 `undefined;` 残留项，保留引号值及值中的等号。无需 AppKey / SecretKey。程序先向网盘验证会话，验证成功才安装并按所选偏好保存登录。
3. 在“全部文件”中点击文件夹名称进入，使用路径框跳转；支持返回上级、分页和当前页文件名筛选。勾选文件或文件夹后点击“下载选中”，“全选本页”包含当前筛选结果中的文件夹。
4. 上传前在设置中填写**已经存在且有权限写入的网盘目录**。Cookie 可选择 `/` 或其它目录；也可以进入目录，再点击设置中的“使用当前浏览目录”。点击“上传文件”多选本地文件。
5. 在“传输列表”查看阶段、字节数、进度和速度。任务顺序执行，可取消和重试。下载取消或断线后保留临时文件；重试会重新获取下载地址并尝试续传。

也可展开“OAuth 授权 / Access Token（可选）”，使用自己应用的 AppKey / SecretKey 进行设备码授权，或输入已有 Access Token。OAuth 的上传目录受应用权限限制，例如 `/apps/你的应用名称`。旧版保存的 OAuth 凭据可以继续读取。

文件夹下载先在后台扫描所有子目录和分页，显示扫描进度；扫描完成后，按“下载目录 / 所选文件夹名称 / 子目录 / 文件”加入队列，同时保留空目录。扫描失败或取消时，此批次不会加入部分文件。可在传输列表“取消目录扫描”或“取消全部”；已开始创建的空目录可能保留。扫描后每个文件沿用独立的进度、取消、重试和断点续传。

传输列表每页显示 100 个任务，可用“上一页任务 / 下一页任务”浏览；后台仍按完整队列顺序执行，“取消全部”作用于所有页面。

已有正常目录会合并使用，已有同名文件仍不会覆盖。目录被文件或子目录链接占用、Windows 名称冲突或不安全路径会停止该批次。单批目录下载上限为 50,000 个文件和目录、目录深度 128 层；超出时请分批选子目录。扫描期间请勿移动或重命名网盘文件。

Cookies 只需在本机输入，不要发到聊天、截图或日志中。默认仅保存在当前会话；Windows 可勾选“记住登录”使用 DPAPI 加密保存。权限和传输速度取决于账号权益及百度接口响应，Cookie 失效时需要重新导入。

## 直链获取与验证

在文件页勾选**一个普通文件**，点击“获取并验证直链”。后台获取临时下载 URL，并以 `Range: bytes=0-0` 检查首字节，结果分为：

- **链接与请求头可用，无需 Cookie**：当前 URL 配合显示的 User-Agent / Referer 已通过验证。
- **需要当前 Cookie 会话**：无 Cookie 请求失败，携带会话通过；复制 URL 到外部工具时还需要自行配置 Cookie。
- **未通过下载验证**：未能确认响应为所选文件，显示不含凭据的错误信息。

结果窗口支持复制 URL 和 UA / Referer，不复制 Cookie。链接只在窗口内保留，不写入设置或日志；退出登录时清除。临时链接可能到期，首字节成功不代表完整内容校验、永久有效或不限速。内置下载队列使用同一解析链路，并继续验证完整文件长度和断点范围。

协议依据、公开接口探测和实测边界见 [直链可行性记录](docs/direct-link-feasibility.md)。尚未使用真实账号 Cookie 验证百度 CDN 下载或网页上传。

## 当前功能与边界

- Cookies 登录、设备码授权、Access Token 连接、OAuth 过期前自动刷新、退出登录。
- 目录浏览、200 项分页、当前页筛选、多文件选择。
- 4 MiB 分片上传，预上传 → 缺失分片上传 → 云端合并；两种登录都使用 `rtype=1` 请求自动改名。Cookie 上传还计算完整 MD5、首 256 KiB MD5 和 CRC32。**上传单文件上限 4 GiB**；下载采用 64 位长度和偏移，不使用此上传上限。
- 单文件、多文件及文件夹递归下载，保留目录结构和空目录；HTTP Range 续传、进度/阶段/速度、取消和手动重试。
- 已有同名本地文件会阻止下载。发布前再次通过文件系统操作防止覆盖；完整文件发布之前始终使用临时文件。
- 普通 JSON 只保存 AppKey、目录和“记住登录”偏好。默认不保存 Cookies、SecretKey、Access Token 或 Refresh Token；Windows 可勾选“记住登录”，用当前用户的 DPAPI 加密后保存。Linux 开发版不保存登录凭据。

不支持文件夹上传、分享链接转存、同步盘或多账号。上传重试从预上传开始，不保存跨进程上传会话。传输历史及队列不保存；退出应用后如需续传下载，请重新选择相同远端文件或文件夹及相同本地目录。

网络连接超时 15 秒，等待响应及单次读取超时 120 秒；持续接收数据的下载不受 120 秒总时长限制。取消在网络/数据块边界检查，等待中的请求最迟在超时后结束。上传合并请求一旦发出，取消或退出不能撤销云端结果；不确定时刷新网盘目录确认。

下载断点保存在目标目录的 `.baidu-<名称哈希>-<文件ID>.part` 和 `.json`，包含远端 ID、路径、大小和版本 MD5，不含登录令牌。远端身份变化或 Range 异常时拒绝拼接。程序验证长度和续传范围，未进行最终文件内容 MD5 校验。遇到断点身份冲突，可只删除对应文件的这两个临时文件，再重试。

配置位置：Windows `%LOCALAPPDATA%\Baidu-Drive-Studio`；Linux `$XDG_CONFIG_HOME/Baidu-Drive-Studio`，未设置时为 `~/.config/Baidu-Drive-Studio`。退出登录会清除 `login.credentials`。字体许可证及依赖许可证随发布包附带。

## 构建

Rust 2024 edition，本次工具链 Rust 1.99.1。完整依赖版本固定于 `Cargo.lock`。

Windows 本机（Rust MSVC 工具链 + Visual Studio C++ Build Tools）：

```powershell
cargo build --locked --release
.\scripts\build-windows.ps1
```

Linux 交叉编译（MinGW-w64）：

```bash
rustup target add x86_64-pc-windows-gnu
cargo build --locked --release --target x86_64-pc-windows-gnu
python3 scripts/package.py
```

Linux 桌面开发：`cargo run --locked`。仅测试 API 和传输逻辑：`cargo test --locked --no-default-features`。

## 验证

```bash
cargo fmt -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --target x86_64-pc-windows-gnu --all-targets -- -D warnings
```

测试使用本地 HTTP 服务及临时文件，覆盖实际协议、下载文件结果、令牌刷新、上传哈希合并和界面状态。源码构建/测试不能替代百度真实账号及 Windows 实机测试。具体记录见 [docs/verification.md](docs/verification.md)。

接口依据：[BaiduPCS-Go 网页/PCS 协议](https://github.com/qjfoidnh/BaiduPCS-Go)、[百度官方 SDK](https://github.com/baidu-netdisk/baidu-drive-sdk-go)、[设备码授权](https://pan.baidu.com/union/doc/al0rwqzzl)、[文件列表](https://pan.baidu.com/union/doc/nksg0sat9)、[上传](https://pan.baidu.com/union/doc/3ksg0s9ye)、[下载](https://pan.baidu.com/union/doc/pkuo3snyp)。

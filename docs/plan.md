# 百度网盘客户端实现计划

使用 writing-plans 技能，当前会话内直接实现。

目标：交付 Windows x64 原生 Rust 客户端及源码。架构和范围见 [design.md](design.md)。依赖固定于 Cargo.lock，网络/文件传输在后台线程，避免影响现有目录。

重点验证：凭据不出现在错误文本；下载不能覆盖用户文件；断点偏移必须严格校验；分片去重仍提交完整有序哈希；退出登录后不能被旧后台消息恢复会话。

1. 建立 `Cargo.toml`、`src/lib.rs`、`tests/protocol.rs`，先通过失败测试确定协议和磁盘行为。
2. 实现 `model.rs` 的 `Credentials`、`RemoteFile`、`Error` 及 Windows 文件名校验；实现 `api.rs` 的设备码、刷新、分页和元数据请求，验证中文路径与错误脱敏。
3. 实现 `transfer.rs` 的 `download`/`upload`，通过本地服务检查请求 Range、200 重启、截断保留、目标冲突、上传分片与 create 参数。
4. 实现 `config.rs` 的 `Settings` 和 Windows DPAPI 凭据存储；JSON 不保存密码和令牌。
5. 实现 `main.rs` / `ui.rs`：中文侧栏、设置/登录、文件列表、顺序传输队列、取消和重试、关闭提示。用 egui 无窗口绘制检查基本状态。
6. fmt、全量测试、Linux/Windows clippy、Windows release；添加 README、许可、构建脚本、打包脚本和验证记录，检查 ZIP 和 SHA256。

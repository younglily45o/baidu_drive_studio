# Cookie 登录与直链实现计划

目标与边界见 [cookies-design.md](cookies-design.md)。直接在当前目录实现，保留 OAuth 和现有发布方式，不修改旁边工具。

1. 先写 `tests/cookies.rs`：Cookie 解析、登录请求、分页、失效脱敏、签名下载、无 Cookie/需会话直链探测、Cookie 上传及 OAuth 回归。
2. `model.rs` 增加向后兼容 Cookie 凭据；`cookies.rs` 负责 Cookie 会话、网页/PCS 请求、签名解析和链接探测；`api.rs` 按模式分派。网络时不持有 UI 使用的凭据锁。
3. `transfer.rs` 增加 Cookie 上传需要的完整哈希及 CRC32，并接入现有顺序队列与原有禁止覆盖下载。
4. `ui.rs` 增加 Cookie 输入及后台验证、单文件直链探测结果、复制链接和退出清除；补 UI 状态测试。
5. 全量测试、格式检查、双平台 clippy、Windows release；更新 README、验证记录、许可及 ZIP，核对包内不含凭据。

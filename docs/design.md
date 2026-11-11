# 百度网盘 Rust Windows 客户端设计

0.2.0 增加 Cookie 会话和直链验证，见 [cookies-design.md](cookies-design.md)；0.3.0 增加文件夹递归下载及文件名左对齐，见 [folder-download-plan.md](folder-download-plan.md)。直链验证边界见 [direct-link-feasibility.md](direct-link-feasibility.md)。以下为 0.1.0 的基础设计，OAuth 仍保留。

用户确认首版覆盖授权登录、文件浏览、上传下载、传输进度。目标 Windows 10/11 x64，中文字体内嵌，独立 EXE。本目录为独立项目，不修改旁边的工具。

采用与当前 Tools 目录一致的 Rust + egui/eframe 原生桌面方案。Tauri 可提供 Web 界面，但增加前端及 WebView2 依赖；Slint 需要另建 UI 工作流。本次选择 egui，由实现者决定界面细节。使用 reqwest blocking + 后台线程，GUI 不执行网络或文件传输；一个顺序队列限制资源占用。

## 功能及边界

- 设备码登录：用户配置自己应用的 AppKey 和 SecretKey，浏览器在百度官方页面登录；也可输入已有 Access Token。支持刷新令牌、退出登录。
- 文件：路径导航、返回上级、分页、当前页文件名筛选、多选文件下载。只显示真实接口数据。账户容量作为可选信息，失败不阻止文件浏览。
- 上传：多选普通文件，4 MiB 分片哈希、precreate、按服务端索引上传、create；合并时保留完整有序 MD5 数组。重名自动改名，不覆盖；目标目录及权限由开放平台授予的范围决定。
- 下载：独立临时文件及身份元数据，校验 HTTP Range 的起点和总长度；服务端返回 200 则从头下载。只有实际长度达到远端声明值才发布最终文件。发布不得覆盖已有文件。
- 传输：顺序队列、阶段、字节数、平均速度、取消/重新开始。下载重试可复用临时文件，上传重试从预上传开始。网络请求有超时，取消在数据块边界生效。关闭前提示未完成传输。
- 认证信息默认只在内存中，Windows 可选使用 DPAPI 保存，绑定当前 Windows 用户。非 Windows 开发环境不落盘令牌。普通 JSON 只保存非敏感设置。
- 非首版范围：分享链接转存、目录递归传输、网盘文件删除、同步盘、多账户、会员提速、上传跨进程断点续传。下载临时文件可跨进程恢复，但队列历史不持久化。

## 模块

`model.rs` 负责数据结构、路径校验、错误及大小格式化；`api.rs` 负责官方协议及令牌刷新；`transfer.rs` 负责下载/上传；`config.rs` 负责非敏感配置和 DPAPI；`ui.rs` 负责展示和任务调度。

## 验证与交付

用本地 HTTP 服务测试真实请求和文件结果，包括认证错误脱敏、URL 编码、分页、Range、截断、目标占用、服务端分片索引、完整哈希列表、取消与登录状态。运行 fmt、全量 tests、Linux/Windows clippy、Windows release 构建，检查 PE 架构及依赖，打包 ZIP 和 SHA256。没有用户凭据时不声明百度账号实测；没有 Windows 环境时不声明 Windows 图形界面实测。

协议依据：百度官方 SDK https://github.com/baidu-netdisk/baidu-drive-sdk-go ，官方文档 https://pan.baidu.com/union/doc/al0rwqzzl 、 https://pan.baidu.com/union/doc/nksg0sat9 、 https://pan.baidu.com/union/doc/3ksg0s9ye 、 https://pan.baidu.com/union/doc/pkuo3snyp 。

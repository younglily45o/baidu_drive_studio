# Cookies 与直链下载可行性记录

检查日期：2026-11-11。0.3.0 沿用 0.2.2 的直链接口链路；新增目录下载逐文件复用此链路。

结论：已在 Rust 客户端实现 Cookie 会话、获取临时直链和首字节探测。参考项目存在对应接口链路，本地协议测试通过，真实下载接口可达并要求登录。本次未使用真实账号 Cookie 做外部验证，尚不能确认具体账号当前可获取直链、无需 Cookie 使用、完整下载成功或传输速度。

## 接口链路

1. 完整 Cookie → `/api/gettemplatevariable` → 验证非空 `bdstoken`。
2. `/api/list` 使用 `num/page` 浏览目录；已选文件通过网页 `/api/filemetas?target=[路径]&dlink=1` 刷新身份和大小，避开需要独立 PCS token 的旧元数据接口。
3. 如果网页元数据已返回 dlink，校验后直接使用。否则在 `/disk/home` 中从同一个 JSON 对象读取 `sign1`、`sign3`、`timestamp`，生成协议要求的签名。
4. 签名回退路径由 `/api/download` 发送签名、时间戳和所选 `fs_id`，取得临时 URL。
5. 无 Cookie、保留 UA/Referer 发送首字节请求；失败时再携带当前会话验证。区分可独立使用、需要会话、未确认可用。

依据：[BaiduPCS-Go prepare.go](https://github.com/qjfoidnh/BaiduPCS-Go/blob/main/baidupcs/prepare.go)、[download.go](https://github.com/qjfoidnh/BaiduPCS-Go/blob/main/baidupcs/download.go)、[Sign2](https://github.com/qjfoidnh/BaiduPCS-Go/blob/main/baidupcs/netdisksign/sign2.go)。网页接口属于兼容适配，网页结构或接口变化时需要调整。

## 0.2.2 下载前失败修复

用户在下载开始前遇到 31030。上游 [问题记录](https://github.com/qjfoidnh/BaiduPCS-Go/issues/492) 将此错误标注为 `pcs token not exist`。原代码在 Cookie 下载前访问旧 PCS 元数据接口，现改用浏览器会话的网页元数据接口；该接口形态可在 [OpenList 的接口适配](https://github.com/OpenListTeam/OpenList/blob/main/drivers/baidu_netdisk/util.go) 中核对。本次只参考协议参数，没有复制其实现代码。

本地回归检查不再发送旧 PCS 元数据请求、直接采用网页 dlink、没有 dlink 时保留签名回退、缺少大小及身份变化时拒绝下载。另用 Linux 稀疏文件验证 7 GiB 以上偏移的续传，并用跨平台 HTTP 测试验证 64 位 Range。没有实际下载用户报告的 7.1 GiB 文件。

## 此前实际公开请求

从本次开发主机发送不带 Cookie 的请求：

| 请求 | HTTP | 百度错误码 | 可说明的结果 |
|---|---:|---:|---|
| GET `/api/gettemplatevariable?fields=["bdstoken"]` | 200 | -6 | 服务可达；未登录不能获得有效 bdstoken |
| POST `/api/download`，空 fidlist | 200 | -6 | 下载接口可达，拒绝无有效会话的请求 |

未使用他人凭据，也未读取或上传实际网盘文件。这两次请求不能证明通过身份验证后的接口兼容性。

## 本地测试与界面验证入口

本地 HTTP 服务验证实际发送的 Cookie、分页、CSRF 参数、签名及时间戳，以及元数据 → 临时 URL → 内容读取 → 发布本地文件。额外覆盖登录页、403、错误 Range、范围与长度矛盾、跨域跳转拦截、Cookie 不发送到其它 CDN 域、HTTP 链接升级 HTTPS。签名测试包含前置无关 timestamp，避免将不同对象的字段混用。

在 Windows 程序中输入自己的 Cookie，进入目录、选择文件并点击“获取并验证直链”，即可进行真实账号验证。结果只证明当时的首字节响应；完整下载仍由传输队列完成长度与续传检查。需要会话的结果不会被标注成仅 URL 即可使用。

## 限制

- 本次未执行真实账号登录、真实 CDN 文件下载或 Cookie 上传合并。
- 仅允许百度 HTTPS 地址及同样受限的重定向；Cookie 只发送给百度主域，不发送到第三方主机或其它 CDN 域。
- Cookie 不会自动续期，失效需重新从浏览器导入；没有承诺绕过账号权限、会员或服务端速率限制。
- Cookie 上传沿用网页 precreate/create 与分片协议，实际账号的接口兼容性仍待本机验证。上传请求均使用自动改名策略 `rtype=1`，没有覆盖参数 `3`。

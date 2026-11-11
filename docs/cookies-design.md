# Cookie 登录与直链验证设计

用户要求增加 Cookies 登录，并验证直链下载的可能。保留现有 OAuth，增加独立 Cookie 会话。直接用 Rust 适配参考项目的网络协议；相比打包 Go 子进程，此方案复用当前队列、断点、错误处理和 DPAPI，继续交付单 EXE。

完整 Cookie 作为用户主动输入的凭据，至少需要 BDUSS。支持可选 `Cookie:` 前缀、引号值、重复键、值中的等号，并忽略单独 `undefined;` 残留项；拒绝换行/控制字符和非法头，错误不回显输入。`STOKEN` 与 `bdstoken` 不混同。认证信息进入现有 DPAPI 凭据结构；普通配置、日志、发布包不得包含 Cookie。

登录通过 `/api/gettemplatevariable` 获取 bdstoken，成功前不安装会话或持久化。Cookie 文件列表使用 `/api/list` 的 num/page；容量独立查询；元数据使用网页 `/api/filemetas` 按路径查询，并校验身份及大小。Cookie 上传使用网页 precreate/create 和 PCS superfile2，计算完整 MD5、首 256 KiB MD5、CRC32 以及原有分片哈希，不请求覆盖。Cookie 失效时明确提示重新导入。

优先使用网页元数据返回的 dlink；缺少直链时，从 `/disk/home` 同一对象的 sign1/sign3/timestamp 读取签名数据，构造签名并调用 `/api/download`。返回链接只接受百度 HTTPS 主机（开发测试允许显式 localhost），HTTP 链接升级 HTTPS；重定向仍限制可信主机。Cookie 只发往百度主域，不发送给其它 CDN 域或第三方。

文件页对单个选中文件提供“获取并验证直链”。后台先在无 Cookie 的独立请求中读取至多一个字节，检查 HTTP 范围、远端声明大小和非错误正文；若失败再携带会话测试。结果明确区分“仅链接及请求头可用”“需要当前会话”“验证失败”，展示 UA/Referer 和临时链接，允许主动复制链接。探测结果不代表完整文件校验或未来持续可用，链接不写入设置或日志，退出时清除。

现有下载队列复用同一链接解析流程，并继续验证 Range、长度及禁止覆盖。无账号时使用本地服务检查实际 Cookie/参数/签名/分片/直链探测请求，提供界面内的真实账号验证入口，不将本地测试描述成百度实测。

参考：https://github.com/qjfoidnh/BaiduPCS-Go （baidupcs/prepare.go、download.go、internal/panhome/sign.go、netdisksign/sign2.go）。只参考协议，保持独立 Rust 实现；若纳入参考算法表达，附 Apache-2.0 上游许可与归属说明。

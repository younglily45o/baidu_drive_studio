# Third-party notices

The executable embeds Noto Sans CJK Regular (NotoSansCJK-Regular.ttc), licensed
under the SIL Open Font License 1.1. See FONT-LICENSE.txt in the distribution
and assets/FONT-LICENSE.txt in the source tree. The font was reused from this
workspace's Rename Studio assets without modification.

Rust dependencies and their exact versions are recorded in Cargo.lock.
DEPENDENCY-LICENSES.txt contains the license declarations and available license
texts of the Windows dependency graph, collected from the locked crate sources.
Key dependencies include egui/eframe (MIT OR Apache-2.0), reqwest (MIT OR
Apache-2.0), rfd (MIT), serde (MIT OR Apache-2.0), serde_json (MIT OR Apache-2.0),
md5 (MIT), windows-sys (MIT OR Apache-2.0), rustls (Apache-2.0 OR ISC OR MIT),
and ring (ISC AND MIT AND OpenSSL).

Cookie/PCS protocol interoperability and the Sign2-compatible signature implementation
reference qjfoidnh/BaiduPCS-Go (https://github.com/qjfoidnh/BaiduPCS-Go), based on
iikira/BaiduPCS-Go, licensed under Apache-2.0. The upstream license is included
in assets/licenses/BaiduPCS-Go-APACHE-2.0.txt and appended to DEPENDENCY-LICENSES.txt.
The protocol adapter and GUI changes are implemented in Rust for this project.

This project is an independent third-party client. Baidu and Baidu Netdisk
names belong to their respective owners. No official endorsement is implied.

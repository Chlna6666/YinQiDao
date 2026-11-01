//! 认证质询：第一版仅 Cookie 导入（与网易云插件同构）。
//! QQ 扫码登录（ptqrshow → ptqrlogin → check_sig → OAuth → QQLogin 五步链）留待后续版本。

#[allow(unused_imports)]
pub use super::api::{auth_begin, auth_cancel, auth_poll, auth_submit};

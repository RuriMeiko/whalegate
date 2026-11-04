//! Client HTTP dùng chung cho các lệnh gọi "thường" (GitHub, nodejs.org, Cloudflare API, CDP cục bộ, tải ảnh).
//! Một `wreq::Client` duy nhất: tái dùng connection pool và cấu hình TLS thay vì build client mới mỗi lần.
//! Timeout, proxy và redirect policy riêng thì đặt theo từng request (`RequestBuilder::timeout/proxy/redirect`).
//! Không gắn emulation trình duyệt — các đích này không cần giả lập (request tới DeepSeek đi qua client của từng acc).

use std::sync::OnceLock;

pub(crate) fn client() -> &'static wreq::Client {
    static C: OnceLock<wreq::Client> = OnceLock::new();
    C.get_or_init(|| {
        wreq::Client::builder()
            // giống mặc định của reqwest: đi theo tối đa 10 redirect (GitHub release → objects.githubusercontent.com)
            .redirect(wreq::redirect::Policy::limited(10))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("build wreq client dùng chung")
    })
}

pub(crate) fn ua() -> String { format!("whalegate/{}", env!("CARGO_PKG_VERSION")) }

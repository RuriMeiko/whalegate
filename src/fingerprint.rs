// Vân tay trình duyệt của một acc: MỘT nguồn sự thật cho cả hai phía.
//   - phía mạng (wreq): TLS/HTTP2/UA/sec-ch-ua của đúng bản Chrome/Edge đã chọn
//   - phía trình duyệt (CDP): UA + client hints, canvas, WebGL, audio, màn hình, phần cứng, múi giờ
// Mọi giá trị dẫn xuất từ `seed` của acc nên cùng một acc luôn ra cùng một "máy", khác acc ra khác máy.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use wreq_util::{Emulation, Platform, Profile};

/// (profile wreq, số phiên bản chính, là Edge?) — chỉ những bản wreq-util có bảng TLS/header thật.
const BROWSERS: &[(Profile, u32, bool)] = &[
    (Profile::Chrome140, 140, false), (Profile::Chrome141, 141, false), (Profile::Chrome142, 142, false),
    (Profile::Chrome143, 143, false), (Profile::Chrome144, 144, false), (Profile::Chrome145, 145, false),
    (Profile::Chrome146, 146, false), (Profile::Chrome147, 147, false), (Profile::Chrome148, 148, false),
    (Profile::Chrome149, 149, false),
    // Edge: chỉ giữ bản mà sec-ch-ua của wreq-util khớp thuật toán GREASE của Chromium (143/145/146 trong bảng của nó bị chép nhầm)
    (Profile::Edge142, 142, true), (Profile::Edge144, 144, true),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Os { Windows, Mac, Linux }

#[derive(Clone, Debug)]
pub struct Fingerprint {
    pub seed: String,
    pub profile: Profile,
    pub major: u32,
    pub edge: bool,
    pub os: Os,
    pub cores: u32,
    pub memory_gb: u32,
    pub screen: (u32, u32),
    pub avail: (u32, u32),
    pub dpr: f64,
    pub gpu_vendor: &'static str,
    pub gpu_renderer: &'static str,
    /// hạt giống nhiễu canvas/audio — ổn định theo acc
    pub noise: u32,
}

static SEEDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Gắn token → seed của acc (gọi khi nạp store và khi login xong). Token chưa gắn dùng chính nó làm seed.
pub fn bind(token: &str, seed: Option<&str>) {
    if let Some(s) = seed.filter(|s| !s.is_empty()) {
        SEEDS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(token.to_string(), s.to_string());
    }
}

pub fn seed_for(token: &str) -> String {
    SEEDS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).get(token).cloned().unwrap_or_else(|| token.to_string())
}

static DEVICES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Gắn token → device id thật của Chrome riêng của acc (`deepseek-device-id:chat`), để x-device-id khớp cookie đã sinh.
pub fn bind_device(token: &str, device_id: Option<&str>) {
    if let Some(d) = device_id.filter(|d| !d.is_empty()) {
        DEVICES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(token.to_string(), d.to_string());
    }
}

pub fn device_for(token: &str) -> Option<String> {
    DEVICES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).get(token).cloned()
}

pub fn new_seed() -> String { uuid::Uuid::new_v4().to_string() }

struct Rng(u64);
impl Rng {
    fn new(seed: &str) -> Self {
        Rng(seed.bytes().fold(0xcbf29ce484222325u64, |a, b| (a ^ b as u64).wrapping_mul(0x100000001b3)))
    }
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn pick<T: Copy>(&mut self, xs: &[T]) -> T { xs[(self.next() % xs.len() as u64) as usize] }
}

const WIN_GPU: &[(&str, &str)] = &[
    ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce RTX 3060 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce RTX 4060 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ("Google Inc. (NVIDIA)", "ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 SUPER Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ("Google Inc. (Intel)", "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ("Google Inc. (Intel)", "ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0, D3D11)"),
    ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon RX 6600 Direct3D11 vs_5_0 ps_5_0, D3D11)"),
];
const MAC_GPU: &[(&str, &str)] = &[
    ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M1, Unspecified Version)"),
    ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M2, Unspecified Version)"),
    ("Google Inc. (Apple)", "ANGLE (Apple, ANGLE Metal Renderer: Apple M3, Unspecified Version)"),
];
const LINUX_GPU: &[(&str, &str)] = &[
    ("Google Inc. (Intel)", "ANGLE (Intel, Mesa Intel(R) UHD Graphics 630 (CFL GT2), OpenGL 4.6)"),
    ("Google Inc. (NVIDIA Corporation)", "ANGLE (NVIDIA Corporation, NVIDIA GeForce GTX 1660/PCIe/SSE2, OpenGL 4.5.0 NVIDIA 550.120)"),
    ("Google Inc. (AMD)", "ANGLE (AMD, AMD Radeon RX 580 Series (radeonsi, polaris10, LLVM 17.0.6, DRM 3.57, 6.8.0), OpenGL 4.6 (Core Profile) Mesa 24.0.9)"),
];

impl Fingerprint {
    pub fn for_token(token: &str) -> Self { Self::from_seed(&seed_for(token)) }

    pub fn from_seed(seed: &str) -> Self {
        let mut r = Rng::new(seed);
        let (profile, major, edge) = r.pick(BROWSERS);
        let os = r.pick(&[Os::Windows, Os::Windows, Os::Windows, Os::Mac, Os::Linux]);
        let (gpu_vendor, gpu_renderer) = match os {
            Os::Windows => r.pick(WIN_GPU),
            Os::Mac => r.pick(MAC_GPU),
            Os::Linux => r.pick(LINUX_GPU),
        };
        // (màn hình, phần chiếm bởi taskbar/menu bar, dpr)
        let (screen, bar, dpr) = match os {
            Os::Mac => {
                let s = r.pick(&[(1440u32, 900u32), (1512, 982), (1728, 1117), (1470, 956)]);
                (s, 25 + 0, 2.0)
            }
            _ => {
                let (s, d) = r.pick(&[((1920u32, 1080u32), 1.0f64), ((1920, 1080), 1.0), ((2560, 1440), 1.0), ((1536, 864), 1.25), ((1366, 768), 1.0), ((1600, 900), 1.0)]);
                (s, if os == Os::Windows { 40 } else { 27 }, d)
            }
        };
        let cores = match os { Os::Mac => r.pick(&[8u32, 10, 12]), _ => r.pick(&[4u32, 6, 8, 8, 12, 16]) };
        let memory_gb = if cores >= 8 { r.pick(&[8u32, 8, 8, 4]) } else { r.pick(&[4u32, 8]) };
        let noise = (r.next() & 0xffff_ffff) as u32;
        Fingerprint {
            seed: seed.to_string(), profile, major, edge, os, cores, memory_gb,
            screen, avail: (screen.0, screen.1 - bar), dpr, gpu_vendor, gpu_renderer, noise,
        }
    }

    pub fn emulation(&self) -> Emulation {
        let platform = match self.os { Os::Windows => Platform::Windows, Os::Mac => Platform::MacOS, Os::Linux => Platform::Linux };
        Emulation::builder().profile(self.profile).platform(platform).build()
    }

    pub fn user_agent(&self) -> String {
        let plat = match self.os {
            Os::Windows => "Windows NT 10.0; Win64; x64",
            Os::Mac => "Macintosh; Intel Mac OS X 10_15_7",
            Os::Linux => "X11; Linux x86_64",
        };
        let edge = if self.edge { format!(" Edg/{}.0.0.0", self.major) } else { String::new() };
        format!("Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36{}", plat, self.major, edge)
    }

    fn nav_platform(&self) -> &'static str {
        match self.os { Os::Windows => "Win32", Os::Mac => "MacIntel", Os::Linux => "Linux x86_64" }
    }

    /// Brand list đúng thuật toán GREASE của Chromium (seed = số phiên bản chính) — khớp sec-ch-ua wreq gửi.
    pub fn brands(&self, full: bool) -> Vec<(String, String)> {
        const CH: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
        const VER: [&str; 3] = ["8", "99", "24"];
        let s = self.major as usize;
        let grease = (format!("Not{}A{}Brand", CH[s % 11], CH[(s + 1) % 11]), VER[s % 3].to_string());
        let v = if full { format!("{}.0.0.0", self.major) } else { self.major.to_string() };
        let real = (if self.edge { "Microsoft Edge" } else { "Google Chrome" }.to_string(), v.clone());
        let chromium = ("Chromium".to_string(), v);
        const ORDERS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        let o = ORDERS[s % 6];
        let mut list = vec![(String::new(), String::new()); 3];
        list[o[0]] = grease;
        list[o[1]] = chromium;
        list[o[2]] = real;
        list
    }

    #[cfg(test)]
    pub fn sec_ch_ua(&self) -> String {
        self.brands(false).iter().map(|(b, v)| format!("\"{}\";v=\"{}\"", b, v)).collect::<Vec<_>>().join(", ")
    }

    fn user_agent_override(&self) -> Value {
        let (plat, plat_ver) = match self.os {
            Os::Windows => ("Windows", "15.0.0"),
            Os::Mac => ("macOS", "14.5.0"),
            Os::Linux => ("Linux", "6.8.0"),
        };
        let list = |full: bool| self.brands(full).into_iter().map(|(b, v)| json!({"brand": b, "version": v})).collect::<Vec<_>>();
        json!({
            "userAgent": self.user_agent(),
            "acceptLanguage": "vi,en-US;q=0.9,en;q=0.8",
            "platform": self.nav_platform(),
            "userAgentMetadata": {
                "brands": list(false),
                "fullVersionList": list(true),
                "fullVersion": format!("{}.0.0.0", self.major),
                "platform": plat,
                "platformVersion": plat_ver,
                "architecture": if self.os == Os::Mac { "arm" } else { "x86" },
                "model": "",
                "mobile": false,
                "bitness": "64",
                "wow64": false,
            }
        })
    }

    /// Múi giờ IANA theo DS_TZ_OFFSET (giây, UTC+7 = 25200) — cùng nguồn với header x-client-timezone-offset.
    fn timezone_id(&self) -> String {
        let secs: i64 = crate::settings::get("DS_TZ_OFFSET").trim().parse().unwrap_or(25200);
        if secs == 25200 { return "Asia/Ho_Chi_Minh".into(); }
        let h = secs / 3600;
        if h == 0 { "UTC".into() } else { format!("Etc/GMT{}{}", if h > 0 { "-" } else { "+" }, h.abs()) }
    }

    /// Lệnh CDP áp lên một phiên trang (đã kèm sessionId do caller thêm).
    pub fn cdp_commands(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("Page.enable", json!({})),
            ("Emulation.setUserAgentOverride", self.user_agent_override()),
            ("Emulation.setTimezoneOverride", json!({"timezoneId": self.timezone_id()})),
            ("Emulation.setLocaleOverride", json!({"locale": "vi-VN"})),
            ("Page.addScriptToEvaluateOnNewDocument", json!({"source": self.js_patch(), "runImmediately": true})),
        ]
    }

    /// Mã JS chạy trước mọi script của trang: canvas, WebGL, audio, màn hình, phần cứng.
    pub fn js_patch(&self) -> String {
        let langs = r#"["vi","en-US","en"]"#;
        PATCH_JS
            .replace("%NOISE%", &self.noise.to_string())
            .replace("%CORES%", &self.cores.to_string())
            .replace("%MEM%", &self.memory_gb.to_string())
            .replace("%SW%", &self.screen.0.to_string())
            .replace("%SH%", &self.screen.1.to_string())
            .replace("%AW%", &self.avail.0.to_string())
            .replace("%AH%", &self.avail.1.to_string())
            .replace("%DPR%", &self.dpr.to_string())
            .replace("%GV%", &serde_json::to_string(self.gpu_vendor).unwrap())
            .replace("%GR%", &serde_json::to_string(self.gpu_renderer).unwrap())
            .replace("%PLAT%", &serde_json::to_string(self.nav_platform()).unwrap())
            .replace("%LANGS%", langs)
            .replace("%GLPARAMS%", &self.gl_params())
            .replace("%FONTS%", &self.font_list())
    }

    /// Tham số WebGL theo lớp GPU: ANGLE gom giá trị về cùng một bộ cho mỗi backend, nên chỉ cần phân theo OS.
    /// Linux/Mesa để nguyên giá trị thật của máy (không có bảng chắc chắn để giả).
    fn gl_params(&self) -> String {
        match self.os {
            // MAX_TEXTURE_SIZE, CUBE_MAP, RENDERBUFFER, VIEWPORT_DIMS, VERTEX/FRAGMENT_UNIFORM_VECTORS, LINE_WIDTH, POINT_SIZE
            Os::Windows => r#"{"3379":16384,"34076":16384,"34024":16384,"3386":[32767,32767],"36347":4095,"36349":1024,"33902":[1,1],"33901":[1,1024]}"#.into(),
            Os::Mac => r#"{"3379":16384,"34076":16384,"34024":16384,"3386":[16384,16384],"36347":1024,"36349":1024,"33902":[1,1],"33901":[1,511]}"#.into(),
            Os::Linux => "null".into(),
        }
    }

    /// Danh sách font của "máy" theo OS; Linux giữ font thật (null = không lọc).
    fn font_list(&self) -> String {
        let l: &[&str] = match self.os {
            Os::Windows => &["Arial","Arial Black","Bahnschrift","Calibri","Cambria","Cambria Math","Candara","Comic Sans MS","Consolas","Constantia","Corbel","Courier New","Ebrima","Franklin Gothic Medium","Gabriola","Gadugi","Georgia","Impact","Ink Free","Javanese Text","Leelawadee UI","Lucida Console","Lucida Sans Unicode","Malgun Gothic","Microsoft Himalaya","Microsoft JhengHei","Microsoft New Tai Lue","Microsoft PhagsPa","Microsoft Sans Serif","Microsoft Tai Le","Microsoft YaHei","Microsoft Yi Baiti","MingLiU-ExtB","Mongolian Baiti","MS Gothic","MV Boli","Myanmar Text","Nirmala UI","Palatino Linotype","Segoe MDL2 Assets","Segoe Print","Segoe Script","Segoe UI","Segoe UI Emoji","Segoe UI Historic","Segoe UI Symbol","SimSun","Sitka Text","Sylfaen","Symbol","Tahoma","Times New Roman","Trebuchet MS","Verdana","Webdings","Wingdings","Yu Gothic"],
            Os::Mac => &["Arial","Arial Black","Arial Narrow","Avenir","Avenir Next","Baskerville","Big Caslon","Brush Script MT","Chalkboard","Cochin","Copperplate","Courier","Courier New","Didot","Futura","Geneva","Georgia","Gill Sans","Helvetica","Helvetica Neue","Hoefler Text","Impact","Lucida Grande","Marker Felt","Menlo","Monaco","Optima","Palatino","Papyrus","Rockwell","SF Pro","Skia","Tahoma","Times","Times New Roman","Trebuchet MS","Verdana","Zapfino"],
            Os::Linux => return "null".into(),
        };
        serde_json::to_string(l).unwrap_or_else(|_| "null".into())
    }

    /// Kích thước cửa sổ Chrome khi mở cho acc này (vừa trong vùng làm việc của màn hình giả).
    pub fn window_size(&self) -> (u32, u32) { (self.avail.0.min(1440), self.avail.1.min(900)) }
}

const PATCH_JS: &str = r#"(() => {
  if (window.__wgfp) return; Object.defineProperty(window, '__wgfp', {value: 1});
  const NOISE = %NOISE% >>> 0;
  const rng = (s) => () => { s |= 0; s = (s + 0x6D2B79F5) | 0; let t = Math.imul(s ^ (s >>> 15), 1 | s); t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t; return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
  // ---- che dấu vết hàm bị thay: toString trả "[native code]" ----
  const nativeStr = new WeakMap();
  const ts = Function.prototype.toString;
  const fakeTs = function toString() { return nativeStr.has(this) ? nativeStr.get(this) : ts.call(this); };
  nativeStr.set(fakeTs, 'function toString() { [native code] }');
  Object.defineProperty(Function.prototype, 'toString', {value: fakeTs, writable: true, configurable: true});
  const mark = (fn, name) => { nativeStr.set(fn, 'function ' + name + '() { [native code] }'); return fn; };
  const method = (proto, name, make) => {
    const orig = proto[name]; if (typeof orig !== 'function') return;
    const fn = make(orig); Object.defineProperty(fn, 'name', {value: name, configurable: true});
    mark(fn, name);
    Object.defineProperty(proto, name, {value: fn, writable: true, configurable: true, enumerable: false});
  };
  const getter = (proto, name, val) => {
    const g = function() { return val; }; Object.defineProperty(g, 'name', {value: 'get ' + name, configurable: true});
    nativeStr.set(g, 'function get ' + name + '() { [native code] }');
    Object.defineProperty(proto, name, {get: g, configurable: true, enumerable: true});
  };
  // ---- navigator / screen ----
  getter(Navigator.prototype, 'hardwareConcurrency', %CORES%);
  getter(Navigator.prototype, 'deviceMemory', %MEM%);
  getter(Navigator.prototype, 'platform', %PLAT%);
  getter(Navigator.prototype, 'languages', Object.freeze(%LANGS%));
  getter(Navigator.prototype, 'language', %LANGS%[0]);
  getter(Screen.prototype, 'width', %SW%);
  getter(Screen.prototype, 'height', %SH%);
  getter(Screen.prototype, 'availWidth', %AW%);
  getter(Screen.prototype, 'availHeight', %AH%);
  getter(Screen.prototype, 'colorDepth', 24);
  getter(Screen.prototype, 'pixelDepth', 24);
  try { Object.defineProperty(window, 'devicePixelRatio', {get: mark(function() { return %DPR%; }, 'get devicePixelRatio'), configurable: true}); } catch (e) {}
  // ---- canvas 2D: nhiễu cố định theo acc, đủ để đổi hash nhưng mắt không thấy ----
  const jitter = (data, salt, stride) => {
    const r = rng(NOISE ^ salt); const st = stride || 4; const n = Math.floor(data.length / st);
    let done = 0;
    for (let tries = 0; tries < 400 && done < 24; tries++) {
      const p = Math.floor(r() * n) * st;
      const ch = Math.floor(r() * 3);
      // pixel trong suốt (alpha 0) giữ nguyên — đổi nó là dấu hiệu bị can thiệp; buffer không có alpha (stride 3) thì chạm tự do
      if (st === 4 && data[p + 3] === 0) continue;
      data[p + ch] = data[p + ch] ^ 1; done++;
    }
  };
  const noisyCopy = (canvas) => {
    if (!canvas.width || !canvas.height) return null;
    const c = document.createElement('canvas'); c.width = canvas.width; c.height = canvas.height;
    const x = c.getContext('2d'); x.drawImage(canvas, 0, 0);
    const img = origGet.call(x, 0, 0, c.width, c.height); jitter(img.data, canvas.width * 31 + canvas.height);
    x.putImageData(img, 0, 0); return c;
  };
  const origGet = CanvasRenderingContext2D.prototype.getImageData;
  method(CanvasRenderingContext2D.prototype, 'getImageData', (o) => function(sx, sy, sw, sh) {
    const img = o.apply(this, arguments); try { jitter(img.data, sw * 17 + sh); } catch (e) {} return img;
  });
  method(HTMLCanvasElement.prototype, 'toDataURL', (o) => function() {
    let src = this; try { if (this.getContext('2d') || true) { const c = noisyCopy(this); if (c) src = c; } } catch (e) {}
    return o.apply(src, arguments);
  });
  method(HTMLCanvasElement.prototype, 'toBlob', (o) => function() {
    let src = this; try { const c = noisyCopy(this); if (c) src = c; } catch (e) {}
    return o.apply(src, arguments);
  });
  // ---- WebGL / WebGL2: vendor + renderer thật của "card" đã chọn ----
  for (const C of [window.WebGLRenderingContext, window.WebGL2RenderingContext]) {
    if (!C) continue;
    method(C.prototype, 'getParameter', (o) => function(p) {
      if (p === 37445) return %GV%;
      if (p === 37446) return %GR%;
      return o.apply(this, arguments);
    });
    method(C.prototype, 'readPixels', (o) => function() {
      const r = o.apply(this, arguments); const buf = arguments[6];
      if (buf && buf.length) { try { jitter(buf, 7); } catch (e) {} } return r;
    });
  }
  // ---- WebGL: tham số "driver" theo lớp GPU (ANGLE D3D11 trên Windows, ANGLE Metal trên macOS) ----
  const GLP = %GLPARAMS%;
  if (GLP) for (const C of [window.WebGLRenderingContext, window.WebGL2RenderingContext]) {
    if (!C) continue;
    method(C.prototype, 'getParameter', (o) => function(p) {
      if (Object.prototype.hasOwnProperty.call(GLP, p)) { const v = GLP[p]; return Array.isArray(v) ? (p === 3386 ? new Int32Array(v) : new Float32Array(v)) : v; }
      return o.apply(this, arguments);
    });
  }
  // ---- font: chỉ lộ những font mà "máy" này có; font ngoài danh sách đo ra như chưa cài ----
  const FONTS = %FONTS%;
  if (FONTS) {
    const allow = new Set(FONTS.map((f) => f.toLowerCase()));
    const generic = new Set(['serif','sans-serif','monospace','cursive','fantasy','system-ui','ui-serif','ui-sans-serif','ui-monospace','ui-rounded','emoji','math','fangsong','inherit','initial','unset','-apple-system','blinkmacsystemfont']);
    const norm = (f) => f.trim().replace(/^["']|["']$/g, '').toLowerCase();
    const webFonts = () => { const s = new Set(); try { document.fonts.forEach((f) => s.add(norm(f.family))); } catch (e) {} return s; };
    const ok = (f, wf) => generic.has(f) || allow.has(f) || wf.has(f);
    const strip = (css) => {
      const parts = css.split(','); const wf = webFonts();
      const kept = parts.filter((x) => ok(norm(x), wf));
      return kept.length === parts.length ? css : (kept.length ? kept.join(',') : 'sans-serif');
    };
    const splitFont = (f) => { const m = /^(.*?\d+(?:\.\d+)?(?:px|pt|em|rem|%)(?:\/\S+)?)\s+(.+)$/.exec(f); return m ? [m[1], m[2]] : null; };
    // đo qua phần tử có font-family inline (cách các script dò font làm): thay tạm bằng chuỗi đã lọc
    for (const name of ['offsetWidth', 'offsetHeight']) {
      const d = Object.getOwnPropertyDescriptor(HTMLElement.prototype, name); if (!d || !d.get) continue;
      const g = function() {
        const ff = this.style && this.style.fontFamily; if (!ff) return d.get.call(this);
        const c = strip(ff); if (c === ff) return d.get.call(this);
        this.style.fontFamily = c; try { return d.get.call(this); } finally { this.style.fontFamily = ff; }
      };
      Object.defineProperty(g, 'name', {value: 'get ' + name}); nativeStr.set(g, 'function get ' + name + '() { [native code] }');
      Object.defineProperty(HTMLElement.prototype, name, {get: g, configurable: true, enumerable: d.enumerable});
    }
    method(Element.prototype, 'getBoundingClientRect', (o) => function() {
      const ff = this.style && this.style.fontFamily; if (!ff) return o.apply(this, arguments);
      const c = strip(ff); if (c === ff) return o.apply(this, arguments);
      this.style.fontFamily = c; try { return o.apply(this, arguments); } finally { this.style.fontFamily = ff; }
    });
    const withFont = (ctx, o, args) => {
      const sp = splitFont(ctx.font); if (!sp) return o.apply(ctx, args);
      const c = strip(sp[1]); if (c === sp[1]) return o.apply(ctx, args);
      const saved = ctx.font; ctx.font = sp[0] + ' ' + c; try { return o.apply(ctx, args); } finally { ctx.font = saved; }
    };
    for (const m of ['measureText', 'fillText', 'strokeText']) method(CanvasRenderingContext2D.prototype, m, (o) => function() { return withFont(this, o, arguments); });
    // document.fonts.check() của Chrome thật trả true cho mọi font hệ thống nên KHÔNG override (override mới là dấu vết)
  }
  // ---- audio ----
  if (window.AudioBuffer) {
    const seen = new WeakSet();
    method(AudioBuffer.prototype, 'getChannelData', (o) => function() {
      const d = o.apply(this, arguments);
      if (!seen.has(d)) { seen.add(d); const r = rng(NOISE ^ 0x5bd1e995); for (let i = 0; i < d.length; i += 97) d[i] += (r() - 0.5) * 1e-7; }
      return d;
    });
  }
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_per_seed_and_varied_across_seeds() {
        let a = Fingerprint::from_seed("seed-a");
        assert_eq!(a.user_agent(), Fingerprint::from_seed("seed-a").user_agent());
        assert_eq!(a.noise, Fingerprint::from_seed("seed-a").noise);
        let uas: std::collections::HashSet<_> = (0..40).map(|i| Fingerprint::from_seed(&format!("s{i}")).user_agent()).collect();
        assert!(uas.len() > 8, "chỉ {} UA khác nhau trong 40 seed", uas.len());
    }

    #[test]
    fn brand_list_matches_chromium_grease_for_versions_seen_in_wreq() {
        let f = |major, edge| Fingerprint { major, edge, ..Fingerprint::from_seed("x") }.sec_ch_ua();
        // chuỗi sec-ch-ua lấy nguyên từ bảng của wreq-util (Chrome thật)
        assert_eq!(f(140, false), r#""Chromium";v="140", "Not=A?Brand";v="24", "Google Chrome";v="140""#);
        assert_eq!(f(143, false), r#""Google Chrome";v="143", "Chromium";v="143", "Not A(Brand";v="24""#);
        assert_eq!(f(147, false), r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#);
        assert_eq!(f(149, false), r#""Google Chrome";v="149", "Chromium";v="149", "Not)A;Brand";v="24""#);
        assert_eq!(f(142, true), r#""Chromium";v="142", "Microsoft Edge";v="142", "Not_A Brand";v="99""#);
        assert_eq!(f(144, true), r#""Not(A:Brand";v="8", "Chromium";v="144", "Microsoft Edge";v="144""#);
    }

    #[test]
    fn patch_has_no_unfilled_placeholders() {
        for os_seed in ["seed-a", "seed-b", "seed-c", "seed-d", "seed-e", "seed-f"] {
            let js = Fingerprint::from_seed(os_seed).js_patch();
            for ph in ["%NOISE%", "%CORES%", "%MEM%", "%SW%", "%SH%", "%AW%", "%AH%", "%DPR%", "%GV%", "%GR%", "%PLAT%", "%LANGS%", "%GLPARAMS%", "%FONTS%"] {
                assert!(!js.contains(ph), "còn placeholder {ph}");
            }
        }
    }
}

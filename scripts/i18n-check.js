#!/usr/bin/env node
// Kiểm tra độ phủ bản dịch: mọi chữ tiếng Việt tĩnh trong src/ui.html phải có bản en + zh.
// Dùng: node scripts/i18n-check.js
const fs = require('fs'), path = require('path');
const root = path.join(__dirname, '..');
global.window = {};
global.document = {body: null, addEventListener() {}, documentElement: {}, getElementById() { return null; }};
global.localStorage = {getItem() { return null; }, setItem() {}};
global.navigator = {language: 'en'};
global.MutationObserver = class { observe() {} };
eval(fs.readFileSync(path.join(root, 'src/i18n.js'), 'utf8'));
const W = window.WG_I18N;

const html = fs.readFileSync(path.join(root, 'src/ui.html'), 'utf8');
const body = html.slice(html.indexOf('<body>'), html.indexOf('<script>'));
const VI = /[àáảãạăằắẳẵặâầấẩẫậèéẻẽẹêềếểễệìíỉĩịòóỏõọôồốổỗộơờớởỡợùúủũụưừứửữựỳýỷỹỵđĐ]|\b(Dán|Thêm|Huỷ|Lưu|Tải|Nhãn|Cài|Kéo|Sửa|Xoá|Mật|Chưa|Mở|Quét|Dò|Cập)\b/;
const items = new Set();
for (const m of body.matchAll(/>([^<>]+)</g)) items.add(m[1]);
for (const m of body.matchAll(/\b(?:placeholder|title|aria-label)="([^"]+)"/g)) items.add(m[1]);

// alert() của bookmarklet/lệnh console: dịch lúc dựng mã nên cũng phải có bản dịch
const full = html.slice(html.indexOf('<script>'));
for (const m of full.matchAll(/alert\('whalegate: ([^']+)'\)/g)) items.add(m[1]);
for (const m of full.matchAll(/\bT\('([^']+)'\)/g)) items.add(m[1]);

const SKIP = new Set(['Tiếng Việt']); // tên ngôn ngữ trong #langSel (data-nt)
const miss = [];
for (const it of items) {
  const t = it.replace(/&amp;/g, '&').replace(/\s+/g, ' ').trim();
  if (!t || SKIP.has(t) || !VI.test(t)) continue;
  W._setLangRaw('en'); const en = W.tr(t);
  W._setLangRaw('zh'); const zh = W.tr(t);
  if (en === t || zh === t) miss.push(t);
}
// nhãn / gợi ý / nhóm / đơn vị của các mục cài đặt hiện trên dashboard (settings.rs, chỉ các mục show)
const rs = fs.readFileSync(path.join(root, 'src/settings.rs'), 'utf8').split('\n');
const lits = (l) => [...l.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
const setItems = [];
for (const l of rs) {
  if (l.includes('shown(')) {
    const a = lits(l);
    setItems.push(a[1], a[a.length - 2], a[a.length - 1]);                       // nhóm, nhãn hiển thị, gợi ý
    if (l.includes('shown(n(')) setItems.push(a[3]);                              // đơn vị
  } else if (l.includes('show: true')) {
    for (const k of ['group', 'label', 'hint', 'unit']) { const m = l.match(new RegExp(k + ': "((?:[^"\\\\]|\\\\.)*)"')); if (m) setItems.push(m[1]); }
  }
}
const setMiss = [];
for (const t of new Set(setItems.filter((x) => x && VI.test(x)))) {
  W._setLangRaw('en'); const en = W.tr(t); W._setLangRaw('zh'); const zh = W.tr(t);
  if (en === t || zh === t) setMiss.push(t);
}
console.log(`settings.rs: ${new Set(setItems).size} chuỗi, thiếu bản dịch: ${setMiss.length}`);
setMiss.forEach((x) => console.log('  thiếu:', x));
miss.push(...setMiss);

// bảng dịch phải đủ 3 cột
const bad = W.DICT.filter((r) => r.length !== 3 || !r[1] || !r[2]);
console.log(`ui.html: ${items.size} chuỗi, thiếu bản dịch: ${miss.length}; dòng DICT hỏng: ${bad.length}`);
miss.forEach((x) => console.log('  thiếu:', x));
bad.forEach((x) => console.log('  hỏng:', JSON.stringify(x)));
process.exit(miss.length || bad.length ? 1 : 0);

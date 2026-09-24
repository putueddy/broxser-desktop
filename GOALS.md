# Handoff Broxser untuk Claude Code Cloud

Salin seluruh bagian **Prompt handoff** di bawah ke Claude Code Cloud, dengan
repository `putueddy/broxser-desktop` terhubung. File ini dibuat setelah initial
commit [`5bcd5f1`](https://github.com/putueddy/broxser-desktop/commit/5bcd5f1)
berhasil di-push pada 25 September 2026. Selalu baca HEAD terbaru; kode bisa sudah
berkembang setelah snapshot handoff ini.

## Prompt handoff

Anda melanjutkan implementasi **Broxser**, aplikasi desktop internal untuk tim
developer perusahaan. Kerjakan kode, tes, dokumentasi, dan hasil yang dapat direview;
jangan berhenti pada rencana atau mockup. Pertahankan perubahan pengguna dan bekerja
di branch tugas, lalu siapkan PR untuk direview. Jangan force-push atau merge sendiri.

### Misi produk

Bangun alternatif internal untuk workflow utama [Sizzy](https://sizzy.co/): satu
workspace untuk menguji aplikasi web pada beberapa viewport dan session, dengan
navigasi/sinkronisasi, debugging dan capture yang efisien. Tujuan bisnis adalah
mengurangi subscription serta waktu berpindah alat. Jangan menganggap build sendiri
pasti lebih murah; biaya maintenance harus diukur sebelum rollout.

Keputusan pengguna yang wajib dipertahankan:

- **Linux dahulu**, dengan Wayland dan X11 sebagai target kualifikasi. macOS nanti.
- **Rust 1.98.1**, telah dipilih pengguna; workspace minimum Rust 1.98.
- **GPUI** untuk UI native dan **Helium** sebagai runtime browser.
- Produk dirancang agar dapat dirawat sampai 2036 melalui komponen yang bisa diganti,
  data portabel, security updates, tes dan ownership. Bukan membekukan dependency
  selama sepuluh tahun.
- Tidak ada backend, telemetry, akun vendor, atau subscription wajib untuk fitur lokal.
- Tidak mengambil source, aset, branding atau mekanisme lisensi Sizzy.

### Baca sebelum mengubah kode

1. `AGENTS.md` dan `README.md`.
2. `docs/system-design.md` beserta `docs/adr/0001-*.md` sampai `0003-*.md`.
3. `docs/validation.md`, `SECURITY.md`, dan `NOTICE.md`.
4. `Cargo.toml`, `rust-toolchain.toml`, `runtime/helium-linux-x86_64.json`.
5. Keempat crate dan `examples/workspace.json` untuk memahami kontrak yang nyata.

`docs/system-design.md` adalah desain yang dapat direview melalui Git;
`docs/system-design.docx` adalah snapshot tujuh halaman dari template System Design.
Jika lingkungan cloud tidak punya renderer dokumen, perbarui Markdown dan tandai
snapshot Word untuk diselaraskan. Jangan mengaku memverifikasi Word tanpa render.

### Kondisi awal yang sebenarnya

| Modul | Sudah tersedia |
| --- | --- |
| `broxser-core` | Workspace JSON v1, validasi, atomic save, pure sync router opt-in dengan origin/sequence dan scope session |
| `broxser-engine` | Blocking CDP adapter, owned Helium child, profil privat, BrowserContext per session, viewport/touch emulation, screenshot PNG |
| `broxser-desktop` | Window GPUI, sidebar, URL dari konfigurasi, canvas gambar, refresh, zoom, status, background capture dan image-cache cleanup |
| `broxser-cli` | `init`, `validate`, `doctor`, `capture`, folder output unik per run dan `report.json` setelah sukses |
| Infra | Cargo.lock, GPUI 0.2.2, pinned Helium Linux 0.18.1.1 + SHA-256, CI, fixture lokal, ADR dan security notes |

**Gambar di canvas adalah screenshot statis.** Belum ada live web surface, text
input URL, navigation history, klik/typing di halaman, browser scroll sync, DevTools
panel, console aggregator, persistent login atau project picker. Pure sync router
yang ada belum terhubung dengan event browser. Jangan menyebut fitur tersebut selesai.

Setiap capture saat ini membuat browser/session baru. Dua device dengan session
sama berbagi BrowserContext dalam run itu; session berbeda menggunakan context
berbeda. Label `Admin` tidak memberi akses atau login. Ukuran mobile tidak
menjadikan Chromium sebagai Safari atau perangkat fisik.

GPUI bukan web renderer. Tidak ditemukan public embedding SDK Helium pada sumber
yang ditinjau saat foundation; ini hasil penelusuran, bukan jaminan permanen.
Jalur awal adalah proses Helium eksternal lewat CDP. Jangan mengimpor Helium sebagai
widget webview imajiner, mengganti GPUI dengan Electron/Tauri, atau memulai fork
Chromium besar tanpa ADR baru berbasis bukti.

### Bukti dan masalah yang harus diwarisi

- Pada snapshot awal: 12 tes default lulus (8 core, 4 engine), ditambah tes live
  Helium yang menguji cookie scope dan dimensi PNG pada DPR1/DPR2.
- Build native GPUI dengan Rust 1.98.1 lulus. Tiga capture nyata sudah tampil dan
  refresh di window Wayland; screenshot ada di `docs/images/preview.png`.
- CLI diuji menolak overwrite config, mempertahankan hasil run lama saat gagal,
  memisahkan output run, dan membersihkan browser setelah sukses/kegagalan navigasi.
- Clippy ketat default members lulus. Desktop-only Clippy sempat terinterupsi;
  jangan mengubahnya menjadi klaim lulus. Lihat status CI terbaru juga.
- Pengujian otomatis close-during-capture belum memberi bukti kepemilikan proses
  yang andal. Normal close exit 0 teramati; crash, SIGTERM dan cleanup aktif masih
  perlu tes khusus. Jangan menganggap `Drop` berjalan jika proses dibunuh paksa.
- Ada upstream future-compatibility warning `proc-macro-error2 2.0.1`.

**Prioritas reliability:** halaman fixture dengan delay respons dua detik kadang
menghasilkan `Page.navigate` → `net::ERR_ABORTED` pada device kedua. Ini terjadi
di UI dan CLI. Trace menunjukkan reload di target phone saat navigasi tablet masih
berjalan; fixture tidak berisi JavaScript navigasi. Penyebab belum terkonfirmasi.
Eksperimen `Page.stopLoading` sebelum navigasi dan membuat seluruh target lebih
awal tidak menyelesaikannya, sehingga sudah dibuang. Jangan mengulang kedua
eksperimen sebagai solusi tanpa bukti baru. Kode trace eksperimen tidak disimpan
di repository; buat reproducer otomatis yang aman dan tidak merekam URL rahasia.

Issue terpisah yang **sudah diperbaiki**: timeout handshake websocket terlalu
pendek. Adapter memakai deadline 15 detik selama HTTP upgrade, lalu polling
500 ms sesudah websocket terbentuk. Pertahankan pemisahan ini.

### Milestone aktif pertama M0 reliability

Selesaikan stabilisasi foundation sebelum menyebut fitur interaktif siap dipakai.

1. Buat reproducer deterministik untuk multi-target dengan slow HTTP response,
   shared session, navigasi berurutan dan paralel, serta DPR1/DPR2. Gunakan server
   fixture milik tes pada port acak, tidak bergantung server terminal pengguna.
2. Bandingkan Helium baseline dengan Chromium pilihan eksplisit untuk mengisolasi
   perilaku upstream. Catat executable, checksum/version, command, event/loader ID
   yang relevan, dan hasil. Product string `Chrome/...` juga dapat berasal dari
   Helium; jangan pakai string itu sebagai bukti merek binary.
3. Perbaiki hanya setelah penyebab dipahami. Jangan menutupi `ERR_ABORTED` dengan
   retry URL otomatis; navigasi bisa memicu side effect di aplikasi perusahaan.
   Jika ini keterbatasan upstream, siapkan bukti minimal, capability gate dan ADR
   opsi lanjut yang konkret. Jangan diam-diam mengganti runtime.
4. Tambahkan regression test untuk start, timeout, tab/context lifecycle, close
   ketika request aktif dan error parsial. Pisahkan proses/profil tes melalui root
   direktori unik dan identitas child milik tes, agar tes paralel tidak tercampur.
5. Periksa memory/disk retention pada refresh berulang. GPUI global image asset
   cache sebelumnya menahan semua gambar; implementasi sekarang memakai
   `RetainAllImageCache` per capture dan cleanup eksplisit. Jangan hilangkan itu.

Exit criteria M0 reliability: error slow-page dapat dijelaskan dan ditangani tanpa
retry side effect, test dapat diulang, serta child/profile cleanup pada normal/error
paths terbukti. Jika ada gate upstream yang benar-benar tidak bisa diselesaikan di
cloud, kerjakan bagian independen M1, jelaskan batas itu, dan jangan menyatakan M0 lulus.

### Milestone berikutnya M1 browser interaktif

Kerjakan sebagai rangkaian perubahan kecil yang tetap dapat dijalankan. Tuntaskan
satu alur end-to-end sebelum menambah panel lain.

**Runtime yang tetap hidup.** Refactor capture engine menjadi session controller
yang memiliki proses Helium selama workspace aktif, mempertahankan target/context,
dan menerima command dari UI. Pertahankan CLI capture sebagai mode sekali jalan.
Blocking I/O tidak boleh masuk ke thread GPUI. Kontrak domain tetap bebas GPUI/CDP;
payload protokol dan browser lifecycle tetap di adapter.

**Frame transport.** Buktikan CDP screencast atau mekanisme frame yang memadai di
GPUI. Pakai queue berbatas, maksimal satu frame terbaru yang menunggu per device,
ack frame dengan benar termasuk saat frame dibuang, dan hentikan pekerjaan untuk
device tersembunyi. Kelola ukuran, DPR, zoom, GPU texture ownership dan cleanup.
Jangan meluncurkan browser baru per frame atau menumpuk PNG/base64 tanpa batas.
Nyatakan dengan benar apakah ini frame streaming atau native embedded surface.

**Interaksi dasar.** Buat URL input native yang bisa diedit, navigate/reload,
indikator loading/error dan selected device. Tambahkan pointer, wheel dan keyboard
ke target aktif dengan pemetaan koordinat canvas → CSS viewport yang benar.
Verifikasi zoom, DPR, offset, resize dan focus. Clipboard, IME, shortcuts, popups,
download, permissions dan accessibility membutuhkan gate tersendiri; jangan
mengklaim dukungan penuh dari satu click demo.

**Sync yang aman.** Hubungkan navigation dan scroll sync opt-in dalam session
yang sama. Gunakan origin, sequence, navigation generation, replay suppression
dan scroll coalescing. Interaksi tidak otomatis menyeberang session. Jangan
broadcast password, file upload, typing, klik submit atau transaksi secara default.
Saat reconnect, pulihkan konfigurasi, bukan replay aksi pengguna.

Exit criteria M1:

- Satu local fixture tampil pada tiga viewport dan pembaruan halaman terlihat
  tanpa tombol capture manual.
- URL, reload, selected-device input dan scroll bekerja tanpa UI hang.
- Klik dan scroll tepat pada DPR1/DPR2 dan dua ukuran canvas; target tidak tertukar.
- Sync tidak loop, tidak melewati batas session dan tidak memutar ulang aksi lama.
- Target/browser restart, error transport dan penutupan workspace memiliki hasil
  yang terdefinisi; tidak ada worker, frame cache atau profil tes yang bocor.
- Ada command reproduksi, tes yang bermakna, hasil pengukuran serta manual desktop
  checklist untuk hal yang tidak dapat diuji di cloud.

### Roadmap setelah M1

M2 melengkapi persistent sessions dengan penyimpanan credential yang benar, project
workspaces, console aggregation, screenshot/export, permission UX, recovery dan
pilot lima developer selama dua minggu. M3 menyediakan packaging/signing Linux,
update runtime, rollback rehearsal, dukungan distro/GPU dan ownership operasional.
macOS memiliki milestone terpisah setelah Linux layak. Terminal, API client, agents,
cloud collaboration dan plugin marketplace ditunda sampai ada kebutuhan nyata.

Penggantian subscription Sizzy baru layak diusulkan setelah daily workflows tim
teruji dan biaya maintenance dibandingkan dengan penghematan. Catat primary owner
dan backup, security triage/update cadence, format data yang portabel, serta
kemampuan mengganti runtime. Target performa di System Design belum benchmark.

### Batas yang tidak boleh dilanggar

- Pertahankan sandbox browser. Jangan memakai `--no-sandbox`, debug origin wildcard,
  sertifikat diabaikan global, atau membuka CDP ke jaringan.
- Hanya gunakan profil privat milik Broxser. Jangan membuka profil browser pribadi,
  membaca key pengguna, atau mengunggah cookie/token/capture perusahaan.
- Halaman web tidak mendapatkan akses shell/filesystem lewat bridge. Perlakukan
  event, URL, response CDP dan workspace import sebagai input tidak tepercaya.
- JSON v1 memakai `schema_version`, `name`, `url`, `sessions`, `devices`; lihat
  tipe yang sebenarnya. Jangan menambahkan migrasi destruktif atau mengubah versi
  tanpa backup, validasi dan tes forward-version rejection.
- Pertahankan 8-device/24-million-physical-pixel guardrails sampai perubahan
  anggarannya didukung pengukuran. Guardrail piksel bukan hard memory limit.
- Pin dependency untuk reproduksi dan rotasi rutin untuk keamanan. Jangan
  mengganti versi engine sekaligus mengubah banyak fitur tanpa bukti terpisah.
- Pertahankan notices dan review distribusi sesuai komponen. GPUI Apache-2.0;
  kode asli Helium GPL-3.0 dan komponen upstream mempunyai lisensi sendiri.
- Jangan menambahkan framework/service/abstraksi generik hanya untuk kemungkinan
  kebutuhan masa depan. Pilih perubahan terkecil yang memenuhi milestone nyata.

### Cara bekerja di lingkungan cloud

Mulai dari repo terbaru dan branch terpisah. Jangan bergantung pada path `/home/ipei`,
file DMG lokal, desktop Hyprland pengguna, SSH agent pengguna, atau cache Cargo
di mesin pengembang. Gunakan relative paths dan environment variables yang didokumentasi.

```bash
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked -p broxser-desktop -j 2
bash scripts/fetch-helium.sh
BROXSER_TEST_BROWSER="$PWD/.local/helium/helium" \
  cargo test --locked -p broxser-engine \
  live_capture_has_expected_pixels_and_isolated_sessions -- --ignored --nocapture
```

Build pertama GPUI cukup besar. Jika perlu target directory terpisah untuk tes
engine, gunakan `CARGO_TARGET_DIR` yang berbeda; jangan mengunci pekerjaan paralel
pada folder build yang sama tanpa alasan. File binary/profil/capture tetap diabaikan Git.

Pada Ubuntu 24.04, perhatikan AppArmor user namespaces: workflow repo memakai
profile yang hanya berlaku untuk path Helium tertentu. Jika cloud tidak mempunyai
izin atau GPU/display, tetap selesaikan code, unit/contract tests dan headless tests
yang didukung. Laporkan pembatasan lingkungan beserta command/error yang tepat;
jangan menurunkan sandbox atau mengaku telah memeriksa window native.

Jika akses jaringan/dependency atau izin GitHub membatasi suatu operasi, pertahankan
hasil yang sudah dibuat dan lanjutkan pekerjaan independen. Jangan menghapus CI
untuk menghindari scope credential. Initial push lokal memakai SSH karena OAuth
HTTPS tidak memiliki scope `workflow`; kredensial lokal itu tidak tersedia di cloud.

### Format hasil yang saya harapkan

Kerjakan milestone aktif, tes, periksa diff, kemudian buat commit yang fokus dan
PR reviewable jika integrasi cloud mengizinkan. Judul/deskripsi PR harus menjelaskan
perubahan perilaku, validasi, serta risiko atau gate yang masih terbuka.

Jawaban akhir berbahasa Indonesia dan berisi:

1. Kemampuan yang benar-benar berjalan beserta cara mencobanya.
2. File/komponen yang berubah dan keputusan arsitektur yang dibuat.
3. Command pengujian dan hasil; pisahkan passed, failed, skipped dan manual-only.
4. Link branch/PR, masalah yang tersisa dan milestone berikutnya yang konkret.

Perbarui `docs/validation.md` dan checklist di bawah sesuai bukti. Jangan
memindahkan item ke selesai hanya karena implementasinya terlihat masuk akal.

## Checklist misi

- [x] Foundation repo, initial commit dan upstream tersedia.
- [x] Rust 1.98.1, GPUI shell, real Helium capture, schema v1, CI dan System Design.
- [x] M0 reliability — slow-page cancellation dan lifecycle/cleanup teruji.
  Bukti cloud 24 September 2026: penyebab `ERR_ABORTED` (ADR 0004), reproducer,
  tes lifecycle dan cleanup normal/error di `docs/validation.md`. Masih terbuka:
  CI remote untuk commit ini dan cleanup saat proses Broxser dibunuh/crash.
- [ ] M1 — runtime tetap hidup, live frames, input, navigation/scroll sync.
- [ ] M2 — daily workflows, persistent session, debug/capture dan pilot tim.
- [ ] M3 — Linux packaging, security updates, rollback dan operational ownership.
- [ ] Platform lanjutan setelah gate Linux terpenuhi.

# Handoff lanjutan Broxser untuk Claude Code Cloud

Gunakan repository `putueddy/broxser-desktop` dan `main` terbaru. PR #1–#4 sudah
merged; baseline integrasi adalah commit
[`9e98089`](https://github.com/putueddy/broxser-desktop/commit/9e980897b8816cbe021e2a0cf573e27a798141c9),
25 September 2026. Prompt ini menggantikan handoff foundation sebelumnya.

Salin bagian **Prompt untuk Claude Code** berikut, atau minta Claude membaca file
ini langsung dari repository yang terhubung. Jangan mulai dari branch PR lama.

## Prompt untuk Claude Code

Anda melanjutkan Broxser, aplikasi desktop internal untuk tim developer perusahaan
yang menargetkan workflow utama Sizzy. Kerjakan implementasi, tes dan dokumentasi
sampai hasilnya dapat direview; jangan berhenti pada rencana. Mulai dengan milestone
aktif di bawah, lalu buat PR yang fokus. Roadmap sesudahnya adalah urutan backlog,
bukan instruksi untuk memasukkan semuanya ke satu PR. Jangan merge sendiri.

### Baca dan pertahankan fondasi

Baca `AGENTS.md`, `README.md`, `SECURITY.md`, `docs/system-design.md`, semua ADR
di `docs/adr/`, serta bagian terbaru `docs/validation.md`. Cocokkan dengan kode
di HEAD; bagian Foundation/M0 lama adalah catatan historis, bukan status saat ini.

Keputusan pengguna tetap berlaku: **Linux dahulu**, **Rust 1.98.1**, **GPUI** untuk
UI native, **Helium** untuk runtime browser. Horizon produk 2026–2036 berarti
komponen dapat diganti, data portabel, update keamanan rutin dan ownership yang
jelas. Tidak ada backend, akun vendor, telemetry atau subscription wajib untuk
fitur lokal. Jangan mengganti stack atau memulai fork Chromium besar tanpa ADR
berbasis bukti. Jangan mengambil source, aset atau mekanisme lisensi Sizzy.

Baseline sesudah merge:

| Bagian | Kondisi yang sudah berjalan |
| --- | --- |
| Dependency | GPUI 0.2.2, Helium Linux 0.18.1.1, tungstenite 0.30, base64 0.23.1 dalam lockfile, checkout 7.0.1 |
| Domain | Workspace JSON v1 tervalidasi, atomic save, session/device IDs dan aturan sync |
| Runtime | Satu Helium headless per workspace live, BrowserContext per session, profil privat, CDP lokal, queue dan frame berbatas |
| Desktop | Frame live, URL input, Go, reload, pointer/wheel/keyboard, selection, hide/show, zoom dan status |
| Sync | Opt-in di dalam session dan device yang terlihat; trusted link intent, per-activation ID, execution context dan loader yang cocok |
| Capture | CLI dan mode `--static`, PNG berdimensi fisik, folder output unik per run |
| Reliability | Cancellation termasuk partial websocket handshake, normal/error cleanup, extension guard termasuk event sebelum registrasi context |
| Verifikasi | Gabungan terbaru lulus 40 tes lokal dan 14 tes live Helium; regresi UI PR4 lulus X11 software serta Wayland native pada skala layar 112,5% |

Ini **frame streaming CDP ke GPUI**, bukan native embedded webview. Session live
bertahan selama runtime terbuka, tetapi login belum persisten antar-run. Label
`Admin` bukan login otomatis. Viewport mobile tidak mengubah Chromium menjadi
Safari atau perangkat fisik. Produk belum dinyatakan pengganti penuh Sizzy.

Jangan mengulang pembangunan live runtime, URL bar, basic input atau perbaikan
`ERR_ABORTED`: uBlock bawaan Helium sudah diisolasi dari session context (ADR 0004).
Jangan mengembalikan otorisasi sync berbasis “ada tombol ditekan baru-baru ini”,
URL yang dipotong sebelum navigasi, atau input ke device tersembunyi (ADR 0006).
Redirect ke URL final berbeda, SPA/hash/subframe navigation belum dicakup kontrak
sync saat ini; penambahan dukungan itu memerlukan keputusan dan regresi tersendiri.

### Milestone aktif P0 — runtime berhenti ketika aplikasi induk mati

**Masalah yang masih ada:** ketika proses Broxser menerima SIGTERM, SIGKILL atau
crash, `Drop` tidak dapat diandalkan. Helium dapat tetap hidup dengan port CDP
loopback dan profil sementara berisi session. Selesaikan ini sebelum menambahkan
persistent login atau memperluas pilot.

Titik awal: `crates/broxser-engine/src/browser.rs`, `cdp.rs`, `live.rs`, lifecycle
capture, dan jalur close/restart desktop. Domain `broxser-core` tetap tidak boleh
mengenal proses, network, GPUI atau CDP.

1. Buat reproducer subprocess yang deterministik untuk induk mati saat startup,
   request tertahan, live frames aktif dan teardown. Pakai fixture HTTP dan root
   profil milik tes; jangan menguji dengan profil atau browser pribadi pengguna.
2. Tulis ADR singkat sebelum implementasi lintas proses. Bandingkan cara menjaga
   kepemilikan browser ketika induk hilang: misalnya guardian kecil, kanal liveness
   atau fasilitas parent-death Linux melalui API aman. Buktikan perilakunya; jangan
   mengasumsikan menutup CDP pipe otomatis mematikan browser. Signal handler di
   induk saja tidak menyelesaikan SIGKILL.
3. Implementasikan opsi terkecil yang membuktikan cleanup pada live dan capture.
   Tidak ada service sistem yang harus diinstal dan tidak ada proses baru per frame.
   Pertahankan sandbox, identitas PID + start time, batas waktu dan error yang jelas.
4. Berhentikan hanya browser/helper milik instance itu. Hindari `pkill` umum,
   nama executable sebagai bukti ownership, atau wildcard penghapusan profil.
   Tes harus mempertahankan instance lain yang masih aktif.
5. Tangani sisa profil akibat power loss atau seluruh process tree dibunuh melalui
   marker/lease ownership dan recovery yang konservatif pada startup berikutnya.
   Jangan menghapus direktori yang kepemilikannya tidak pasti; jangan mengikuti
   symlink atau menganggap PID yang dipakai ulang sebagai instance lama.
6. Recovery memulihkan konfigurasi, tanpa replay klik, typing, form, auth, payment
   atau navigasi yang belum jelas hasilnya. Membuka workspace dan tombol Restart
   tetap merupakan aksi eksplisit pengguna yang memuat URL sekali.

**Kriteria selesai P0:**

- Normal close, error, cancel, SIGTERM dan SIGKILL induk diuji pada live/capture.
- Sesudah induk hilang, proses browser/helper miliknya dan endpoint CDP berhenti
  dalam batas yang didokumentasikan; target awal lima detik, ukur hasil sebenarnya.
- Profil dibersihkan oleh pemilik lifecycle yang masih hidup, atau dinyatakan
  stale dan dibersihkan aman pada startup berikutnya untuk kasus seluruh tree mati.
- Tidak ada proses/profil instance lain atau file workspace pengguna yang berubah.
- UI tidak membeku selama shutdown. Tes menguji keadaan aktif, bukan hanya browser
  yang sudah berhenti, dan membedakan proses hidup dari zombie/PID yang dipakai ulang.
- Semua regresi lama tetap lulus, termasuk handshake terfragmentasi, cancellation,
  extension ordering, trusted link sync, URL panjang dan hidden input.
- README, SECURITY, ADR dan validation menyebut jaminan serta keterbatasan tepat;
  jangan menyatakan cleanup SIGKILL lulus hanya berdasarkan `Drop` atau SIGTERM.

Selesaikan milestone ini sebagai PR pertama. Jika muncul batas upstream/platform
yang nyata, sertakan reproducer dan keputusan yang konkret, lalu lanjutkan bagian
independen yang dapat diselesaikan. Jangan menandai pekerjaan blocked sebagai done.

**Status P0 (25 September 2026, menunggu review PR):** guardian per browser, lease
profil dan recovery stale profile diimplementasikan menurut
[ADR 0007](docs/adr/0007-browser-ownership-after-owner-death.md). Reproducer
subprocess (fake browser dan Helium), tes CLI dan smoke X11 membuktikan cleanup
38–105 ms sesudah SIGKILL, SIGTERM, `abort()` atau Ctrl+C induk, termasuk saat
teardown; instance lain dan file workspace tidak berubah. Profil kini mode 0700.
CI run #29 menemukan helper Helium yang menulis ulang profil sesudah dihapus;
semua jalur cleanup kini menunggu sampai tidak ada proses yang menyebut profil.
Bukti dan batas ada di `docs/validation.md`. Sisa di luar lingkup PR itu: direktori
preview mode statis dan direktori socket `org.chromium.Chromium.*` masih tertinggal
(yang kedua juga pada close normal), serta kualifikasi skenario kill di Wayland,
GPU fisik, distro dan kernel perusahaan.

### Backlog sesudah P0

| Urutan | Pekerjaan | Hasil atau gate yang diperlukan |
| --- | --- | --- |
| P1.1 | Deadline command dan navigasi live | Audit pending command async yang belum punya expiry; target yang macet berakhir pada status timeout/cancel yang jelas, state tetap berbatas, device lain tetap berjalan, tanpa retry side effect |
| P1.2 | Restart dan state UI | Reproduce double-Restart, close saat restart dan decode frame lama; serialisasi transisi, beri identitas generation, jangan meluncurkan runtime baru setelah close atau menampilkan frame dari runtime lama |
| P1.3 | Keyboard, IME dan clipboard | Identitas key-down/up yang benar lintas layout/focus, composition/caret, shortcut aplikasi tetap terpisah; paste hanya atas aksi pengguna ke target terlihat; jangan melemahkan hidden-input suppression |
| P1.4 | Kualitas dan penggunaan resource | Ukur p95 input latency, CPU/GPU/RAM, startup, frame age/drop dan sesi beberapa jam; perbaiki DPR/HiDPI, resize/multi-monitor scale serta 3/8 device berdasarkan pengukuran |
| P1.5 | Navigasi aplikasi modern | Definisikan dukungan redirect, hash/SPA dan subframe; pertahankan bukti intent dan scope session, tambahkan tes navigasi yang dibatalkan/superseded dan script-generated events |
| P1.6 | Interaksi browser yang belum didukung | Dialog, popup, permissions, download/upload, touch, drag/drop, accessibility; pecah per kapabilitas, buat keputusan pengguna eksplisit dan kegagalan terlihat, jangan auto-accept |
| P1.7 | Kesesuaian hasil QA dengan browser pengguna | Ukur pengaruh fingerprint/privacy defaults dan perbedaan headless, Helium biasa serta Chromium pembanding; dokumentasikan perbedaan sebelum menyatakan hasilnya representatif |
| P2.1 | Batas penyimpanan dan kredensial | Putuskan isolasi NSS/certificate store sebelum persistent session; browser masih membuka NSS pengguna. Tangani corporate CA secara eksplisit, tanpa menyalin private key atau kredensial personal |
| P2.2 | Workspace dan session permanen | UI kelola proyek/device/preset, restore konfigurasi, persistent login dengan desain profil/secret store dan migrasi yang teruji; portable JSON tetap bebas cookie/token/password |
| P2.3 | Workflow debugging | Console/error aggregation per device/session, screenshot dan export yang berguna untuk bug report; redaksi data sensitif dan retention yang jelas |
| P3.1 | Distribusi dan perawatan Linux | Packaging/signing, checksum/SBOM/notices, update Helium yang dapat dikualifikasi, rollback rehearsal, owner utama dan backup |
| P3.2 | Pilot perusahaan | Lima developer selama dua minggu adalah rencana awal, bukan hasil; ukur daily workflow parity, waktu yang dihemat, failure rate dan beban maintenance sebelum mengganti subscription |
| Sesudah gate Linux | macOS dan platform lain | ADR, packaging, input/rendering dan verifikasi platform tersendiri; jangan menganggap kompilasi sebagai dukungan penuh |

Untuk P1.1/P1.2, konfirmasi masalah melalui kode dan reproducer sebelum mengubah
kontrak: per-operation timeout pada bootstrap/capture bukan jaminan deadline pada
semua command live atau keseluruhan job. Respons yang hilang juga tidak membuktikan
bahwa aksi web belum berjalan. Jangan mengirim ulang aksi demi membuat tes lulus.

Pada P2, jangan sekadar mengekspor cookies menjadi JSON dan menamakannya persistent
session. Jelaskan implikasi off-the-record BrowserContext, isolasi storage, migrasi,
enkripsi/keyring serta penghapusan data. Pertahankan default ephemeral sampai gate
keamanan dan lifecycle selesai. Terminal, API client, AI agents, cloud collaboration
dan plugin marketplace menunggu kebutuhan nyata setelah workflow utama stabil.

### Invariant dan batas kerja

- UI tetap GPUI; runtime tetap Helium dengan adapter yang bisa diganti. Blocking
  I/O, parsing CDP dan browser ownership tetap di engine, bukan thread UI/core.
- Sandbox browser selalu aktif, debug transport lokal dan profil hanya milik
  Broxser. Jangan menambahkan `--no-sandbox`, wildcard debug origins, global TLS
  bypass atau perubahan kernel/system-wide agar tes lewat.
- Halaman web dan eventnya tidak tepercaya. Binding isolated world tidak boleh
  menjadi API filesystem/shell. Pertahankan context, activation ID, URL dan loader
  matching; partial handshake/navigasi tidak dikirim ulang otomatis.
- Hide melarang input, individual reload dan sync. Global Go tetap memuat semua
  device sekali atas aksi eksplisit pengguna. Jangan mengaktifkan hidden device
  sebagai target keyboard diam-diam.
- Pertahankan format v1 dan 8-device/24-million-physical-pixel guardrails. Perubahan
  schema butuh backup, validasi, migrasi dan penolakan versi masa depan yang teruji.
- Pin toolchain/dependency untuk reproduksi lalu rotasi terencana; jangan membekukan
  browser lama demi kompatibilitas. Warning `proc-macro-error2` masih perlu ditinjau
  dalam pembaruan dependency yang terpisah dan terukur.
- Pertahankan lisensi/notices komponen dan review distribusi. Jangan menetapkan
  owner perusahaan, biaya, lisensi baru atau pencapaian performa tanpa data/mandat.
- Gunakan perubahan kecil dan dapat direview, dengan ownership file yang jelas
  bila memakai agen paralel. Hindari framework atau abstraksi spekulatif.

### Verifikasi dan keterbatasan cloud

Mulai dari clone/HEAD terbaru, periksa working tree, lalu buat branch tugas baru.
Jangan bergantung pada `/home/ipei`, `/tmp` sesi sebelumnya, DMG macOS, cache lokal
atau SSH agent pengembang. Dependencies dan runtime tersedia melalui instruksi repo.

```bash
rustup toolchain install 1.98.1 --profile minimal --component rustfmt --component clippy
bash scripts/check.sh
cargo build --locked -p broxser-desktop -j 2
bash scripts/fetch-helium.sh
BROXSER_TEST_BROWSER="$PWD/.local/helium/helium" \
  cargo test --locked -p broxser-engine -- --ignored --test-threads=2 --nocapture
```

Jalankan tes live sebagai user biasa dengan sandbox aktif. Ikuti contoh AppArmor
yang dibatasi pada executable uji di workflow Ubuntu; jika runner tidak mendukung
namespace/display/GPU, laporkan command/error dan batasnya tanpa menurunkan sandbox.
`scripts/desktop-smoke.sh` memerlukan X11/Xvfb, xdotool dan binary yang sudah dibangun.
Perubahan GUI memerlukan window Linux nyata; Xvfb membuktikan jalur window/input,
bukan performa GPU fisik. Bukti Wayland 112,5% yang sudah ada bukan kualifikasi
seluruh compositor, layout keyboard, monitor atau aplikasi perusahaan.

Gunakan fixture HTTP milik tes pada port acak, root profil privat dan identitas
proses yang tepat. Jangan menguji dengan akun produksi atau mencatat token,
cookies, URL rahasia maupun capture perusahaan di CI. Periksa failed/skipped
tests dan regresi seluruh pipeline; jangan hanya memilih tes baru yang lulus.

`docs/system-design.md` dan ADR adalah sumber desain yang aktif. Snapshot DOCX
masih perlu diselaraskan; lakukan melalui workflow template/render bila tersedia.
Jika renderer tidak tersedia di cloud, tandai gap itu dan jangan mengaku sudah
memeriksa dokumen secara visual. Tidak perlu memblokir implementasi independen.

### Hasil yang harus diserahkan

Implementasikan P0, jalankan validasi yang sesuai, lalu buat commit dan PR fokus
untuk review. Jangan force-push, merge sendiri, menghapus CI demi scope credential,
atau memasukkan unrelated cleanup. Perbarui status backlog hanya berdasarkan bukti.

Jawaban akhir berbahasa Indonesia: perilaku sebelum/sesudah, file/ADR yang berubah,
command dan hasil tes dengan pemisahan passed/failed/skipped/manual-only, link PR,
risiko tersisa serta milestone berikutnya. Jika sesi berhenti sebelum selesai,
simpan checkpoint yang dapat dilanjutkan, bukan klaim bahwa seluruh misi tuntas.

## Checklist saat handoff

- [x] PR #1–#4 merged; dependency dan implementasi M1 berada di main.
- [x] Frame live, input dasar, trusted link/scroll sync dan keenam perbaikan review.
- [x] Gabungan source diuji: 40 tes lokal, 14 tes live Helium; bukti X11/Wayland tersedia.
- [x] P0 — cleanup browser/CDP/profil ketika induk mati, serta stale-profile recovery
  (ADR 0007; PR menunggu review; sisa temp dir non-profil tercatat di status P0).
- [ ] P1 — deadline live, transisi restart, input lengkap dan resource/performance gates.
- [ ] P2 — isolasi penyimpanan, persistent session, workspace UI dan debugging harian.
- [ ] P3 — packaging, update/rollback, ownership dan pilot perusahaan.
- [ ] Platform lanjutan setelah gate Linux terpenuhi.

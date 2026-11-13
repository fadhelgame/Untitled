# Kanvas

Editor desain native yang ringan untuk Windows: canvas infinite, frame tanpa batas, Google Fonts.

## Download

**[⬇ Download Kanvas.exe](https://github.com/fadhelgame/Untitled/raw/master/Kanvas.exe)** (Windows 64-bit, ±13 MB)

Tidak perlu instalasi. Download, lalu klik dua kali `Kanvas.exe`.

> Saat pertama dibuka, Windows mungkin menampilkan "Windows protected your PC" karena aplikasinya belum ditandatangani. Klik **More info → Run anyway**.

Catatan:

- Google Fonts membutuhkan koneksi internet saat font diunduh pertama kali.
- Fitur *Remove background* membutuhkan Codex CLI yang terpasang di komputer; fitur lain berjalan tanpa itu.

## Build dari kode sumber

Butuh [Rust](https://rustup.rs).

```bash
cargo build --release
```

Hasilnya ada di `target/release/kanvas.exe`.

---
gorev: DAPP-121
tip: feature
platform: Windows
---
# DAPP-121 — Task Manager names and icons for every Desk-Remote executable

Bitti: tüm exe'ler (panel, ajan, tailnet helper, relay) artık işine uygun ad + Desk-Remote simgesi + VERSION'dan sürüm taşıyor; adlar tek dosyadan geliyor. Task Manager'daki GÖRSEL gruplama doğrulanamadı (aşağıya bak).

## Ne yapıldı
- Yeni merkezi crate `crates/desk-exe-identity` + `identity.json`: her exe'nin Task Manager adı tek yerde. Kullanıcılar: ajan/relay build.rs (simge + VERSIONINFO gömer), `scripts/build-tailnet-helper.ps1` (Go helper, goversioninfo v1.4.1 ile .syso), servis görünen adı (`scm.rs`, "Desk-Remote Agent"). Sürüm `VERSION` dosyasından.
- Adlar: panel "Desk-Remote", `DeskRemote-Ajan.exe` "Desk-Remote Agent", `desk-tailnet-helper.exe` "Desk-Remote Tailnet Helper", `desk-relay.exe` "Desk-Remote Relay"; hepsinde ProductName "Desk-Remote". Test: adlar birbirinden farklı, panel adı tauri.conf.json productName ile aynı.
- Servis görünen adı "Desk-Remote Service" → "Desk-Remote Agent".
- `crates/desk-service-core/build.rs` artık Go helper'ı yalnız damgalı betikle derliyor (eski, damgasız önceden-derlenmiş exe'ye düşmüyor).

## Kanıt
Önce (ölçüm): `desk-tailnet-helper.exe` FileDescription='' ProductName='', `DeskRemote-Ajan.exe` ''/'' , `desk-relay` yok.
Sonra (release, VersionInfo + ExtractAssociatedIcon):
```
DeskRemote-Ajan.exe     | desc='Desk-Remote Agent'            | prod='Desk-Remote' | ver=0.1.0 | icon=32x32
desk-relay.exe          | desc='Desk-Remote Relay'            | prod='Desk-Remote' | ver=0.1.0 | icon=32x32
desk-panel.exe          | desc='Desk-Remote'                  | prod='Desk-Remote' | ver=0.1.0 | icon=32x32
desk-tailnet-helper.exe | desc='Desk-Remote Tailnet Helper'   | prod='Desk-Remote' | ver=0.1.0 | icon=32x32
```
Canlı süreç ağacı (yeni panel, 9 sn, sonra kapatıldı): panel [26888] 'Desk-Remote' → desk-tailnet-helper.exe [17572] parent=26888 desc='Desk-Remote Tailnet Helper' → conhost. AppData'ya açılan kopya da damgalı (SHA farkıyla otomatik yenilendi).
Desk Helper ölçümü: 15 süreç, hepsi tek üst süreç (21540, ebeveyni explorer) altında, aynı exe, FileDescription "Desk Helper", hepsi aynı simge.
Gates: 9/9 geçti (fmt, clippy, birim testler, kopya, ağ izolasyonu, renk, deny, go fmt/vet). Commit e6e6e63, push yok.

## Doğrulanamayanlar (dürüst not)
- **Görev Yöneticisi karesi alınamadı**: Taskmgr yönetici yetkisinde çalışıyor; yetkisiz süreçten tıklama/UIA engelleniyor (UIPI), Süreçler sayfasına geçemedim. Bu yüzden Windows'un gruplama kuralını (ebeveyn-çocuk mü, pencere mi) KANITLAMADIM; yalnız şunu ölçtüm: tailnet helper zaten panelin alt süreci ve artık adı/simgesi var. Lütfen yeni kurulumdan sonra bir kare gönderin.
- **WebView2**: `msedgewebview2.exe` Microsoft'un exe'si; adı/simgesi bizim değiştirebileceğimiz bir şey değil. Ayrı "WebView2 Manager" satırı gruplanıyor mu bilinmiyor (yukarıdaki kare olmadan kanıtsız); uydurma çözüm yapılmadı.
- **Oturum yardımcısı** aynı `DeskRemote-Ajan.exe` dosyası (`helper` alt komutu; tek-exe mimarisi, self-update buna bağlı) olduğundan adı "Desk-Remote Agent". Ayrı "Session Helper" adı için ayrı exe gerekir (mimari değişiklik, ürün kararı).

## Değişen dosyalar
Cargo.toml/lock, crates/desk-exe-identity/*, apps/desk-service/{build.rs,Cargo.toml}, apps/desk-relay/{build.rs,Cargo.toml}, crates/desk-service-core/{build.rs,Cargo.toml,src/scm.rs}, scripts/build-tailnet-helper.ps1, apps/desk-tailnet-helper/.gitignore, docs/architecture/folder-layout.md.

## Riskler
Go helper derlemesi artık `go run goversioninfo@v1.4.1` için ağ/modül önbelleği ister (ilk seferde). Servis görünen adı değişti: kurulu eski servisler yeniden kurulana dek eski adı gösterir.

## Platform
Windows 11 doğrulandı. Diğer platform yok (Windows-only ürün).

## Sonraki adım
Yeni kurulumda Task Manager karesi (Lena/patron); oturum yardımcısı için ayrı exe isteniyorsa karar.

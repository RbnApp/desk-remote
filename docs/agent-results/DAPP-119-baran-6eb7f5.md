---
gorev: DAPP-119
tip: bug
platform: Windows
---
# DAPP-119 — Bağlandı balonu yalnız ilk bağlanışta + başlangıç

Bitti: Balon panel başına yalnız ilk bağlanışta çıkıyor (kalıcı); taşınabilir ve /CURRENTUSER kurulumlar artık oturum açılışında başlıyor. Gerçek servis kurulumu/reboot koşulmadı.

## Ne yapıldı
- Balon: `Hub::record_peer_connected` (hub.rs) — denetim satırı her seferinde, balon yalnız ilk seferde. Kimlik = eşleşmiş panelin device_id'si; `ServiceConfig.announced_panels` (desk-config, service.json) içinde kalıcı. Eşlemeyi kaldırınca kayıt silinir, yeniden eşleşme tekrar bir kez duyurulur. Yeni metin/yeni kayıt biçimi yok.
- Başlangıç ölçümü: kurulu yol (agent.nsi → `install`) servisi AutoStart + gecikmeli kuruyor (scm.rs:157,193), oturum yardımcısını servis başlatıyor → reboot'ta geri gelir. Çalışmayan iki yol bulundu: (a) `DeskRemote-Ajan.exe` çift tık (taşınabilir) ve (b) `/CURRENTUSER` kurulum — ikisinde servis yok, hiçbir şey oturumda başlamıyordu.
- Düzeltme: yeni `startup.rs` — çift tıkta HKCU\...\Run\DeskRemoteAgent (`run --console --portable --data-dir <klasör>`, UAC istemesin diye açık argümanlı); NSIS /CURRENTUSER aynı girdiyi yazar, kaldırıcı siler.

## Kanıt
- `cargo test -p desk-service-core`: yeni `peer_balloon_is_announced_once_per_panel_and_survives_restart` ok (ilk=true, ikinci=false, yeni Hub örneği hâlâ false, unpair sonrası true, service.json içeriği doğrulandı); `startup::command_quotes_both_paths...` ok.
- `scripts/gates.ps1`: ALL 9 AUTOMATED GATES PASSED.
- Commit f309145, `git status` temiz.

## Değişen dosyalar
apps/desk-service/src/main.rs, crates/desk-config/src/model.rs, crates/desk-service-core/src/{engine,host,hub,lib,startup}.rs, packaging/agent.nsi

## Riskler
- Taşınabilir Run girdisi yönetici yükseltmesi yapmaz (UAC her oturumda çıkmasın diye); yönetici pencereleri için kurulu servis yolu önerilir.
- Gerçek servis kurulumu, reboot ve Run anahtarının fiilen yazılması bu PC'de koşulmadı (patronun canlı ekranı); kanıt kod + birim testi.

## Platform
Windows'ta derlendi/test edildi. Kod yalnız Windows (registry, SCM); mac/Linux'ta bu ajan yok.

## Sonraki adım
DAPP-118.

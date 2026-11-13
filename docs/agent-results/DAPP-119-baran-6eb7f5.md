---
sözleşme: 3
görev: TASK-MWEZGVURLRNQ0
ajan: baran-6eb7f5
motor: claude
tip: hata
okur: ekip
hüküm: Bitti: "Eşleşmiş panel bağlandı" balonu artık her bağlantı oturumunda bir kez çıkıyor, aynı oturumda tekrar çıkmıyor; önceki kalıcı "ilk kez" kaydı kaldırıldı.
platform: Windows doğrulandı (birim + gates 9/9). Gerçek servis kurulumu/reboot koşulmadı (yasak). Kod yalnız Windows.
sürüm: 3
tarih: 2026-11-13T13:22:17.734Z
---

## Ne yapıldı
Durum: önceki işte kalıcı "ilk kez" kaydı yapmıştım (`ServiceConfig.announced_panels`); yeni kurala çevirdim, kalıcı kayıt tamamen silindi.

Kök neden: `engine.rs::serve_viewer` her doğrulanmış TCP bağlantısında balonu tetikliyordu; aynı oturumdaki yeniden bağlanma ve ek bağlantılar tekrar balon çıkarıyordu.

Düzeltme (`crates/desk-service-core/src/hub.rs`): cihaz başına canlı bağlantı sayısı + son kopma zamanı tutuluyor. Balon yalnız canlı bağlantı yokken ve son kopmadan ≥120 sn (`SESSION_GRACE`) geçmişse çıkıyor. Ek bağlantı / kısa kopma aynı oturum. `engine.rs` bağlantı bitince `peer_link_closed` çağırıyor. `host.rs`'ten `set_announce_store` ve `desk-config` alanı çıkarıldı.

Başlangıç (madde 2) önceki işteki gibi: kurulu servis gecikmeli otomatik; taşınabilir ve /CURRENTUSER ajan HKCU Run ile oturum açılışında başlıyor.

## Kanıt
- Birim test `peer_balloon_is_announced_once_per_connection_session`: ok.
- `scripts/gates.ps1`: 9/9 PASS.
- Commit 8bb1b35.

## Riskler
120 sn süresi tahmin, istenirse değişir. Gerçek tepsi balonu bu PC'de canlı denenmedi.

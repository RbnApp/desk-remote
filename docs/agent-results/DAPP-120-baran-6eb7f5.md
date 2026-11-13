# DAPP-120 — Desk-Remote panel: idle CPU and periodic freeze

Hüküm: Bitti: kök neden düzeltildi (LanBrowser her taramada mDNS daemon'ını kapatmıyordu), ölçümle doğrulandı, Gates 9/9.

## Ne yapıldı
- Kök neden: `crates/desk-transport/src/discovery.rs` `LanBrowser::browse` her çağrıda yeni `ServiceDaemon` açıyor, `shutdown()` çağırmıyordu. mdns-sd 0.13.11'de `Drop` yok; daemon iş parçacığı ve çoklu-yayın soketleri her tarama (8 sn) sızıyordu. Her yeni daemon tüm arayüzlerde 5353'e bağlanıp multicast'e katılıyor (periyodik sistem takılması); sızan daemon'lar IP-kontrol/yeniden gönderim zamanlayıcılarıyla CPU'yu artırıyor.
- Düzeltme: `browse` sonunda ve hata yolunda `daemon.shutdown()`.
- `transport.rs`: `REFRESH` 8 sn → 30 sn (istenen en az 30 sn).
- UI: list/pair/settings/incoming zamanlayıcıları `document.hidden` iken iş yapmıyor.

## Kanıt (release, bağlantı yok, 120 sn, aynı makine; CPU = 5. sn'den sonraki fark)
Eski:  t=20s cpu 0,0 thr 34 hnd 567 · t=60s 0,4 / 35 / 596 · t=100s 1,3 / 39 / 648 · t=120s 2,0 s / 41 / 674 (CPU hızlanarak artıyor, iş parçacığı+handle sızıyor)
Yeni:  t=20s 0,0 / 30 / 489 · t=60s 0,0 / 27 / 466 · t=120s 0,0 s / 27 / 466 (düz)
Eski binary'de büyüme ivmeli olduğundan 1,5 saatte ~1823 s CPU ile uyumlu. Gates: `scripts/gates.ps1` → ALL 9 AUTOMATED GATES PASSED (fmt `cargo fmt --all` sonrası).

## Değişen dosyalar
crates/desk-transport/src/discovery.rs, apps/desk-panel/src/backend/transport.rs, apps/desk-panel/ui/js/{incoming,list,pair,settings}.js

## Riskler
- 10 sn aralıklı donmayı `donma.ps1` ile ayrıca koşmadım (patronun PC'sini yormamak için); sızıntı ve 8 sn'lik daemon açma döngüsü giderildi, donma izleyicisi patron tarafında teyit etmeli.
- Gizliyken UI durdurma `document.hidden`'a dayanıyor (WebView2 küçültülünce true); gerçek pencerede görsel teyit yapılmadı.
- Eşleşmiş PC listesi durumları artık 30 sn'de bir tazeleniyor (pair/bağlan eylemleri `nudge` ile anında).
- `desk-tailnet-helper` 2 sn Go ticker'ı değişmedi (yalnız tailnet açıkken çalışır, ölçümde sorun görülmedi).

## Platform
Windows 11 doğrulandı. Proje yalnız Windows hedefli (AGENTS.md #6); mac/Linux yok.

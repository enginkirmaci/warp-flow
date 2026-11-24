# Flow motoru — karar gerektirmeyen düzeltmeler (2026-11-24)

Bu belge, [flow-engine-review-2026-11-24.md](flow-engine-review-2026-11-24.md)
incelemesindeki bulgulardan **karar gerektirmeyenlerin** uygulanmasını ve
kalan işleri kaydeder. İnceleme belgesindeki satır numaraları `5bf5a54`
sürümüne aittir; bu belge değişiklik sonrası durumu anlatır.

## 1. Uygulanan düzeltmeler

### P0-1 — "Hayır" artık onay sayılmıyor

- `run.rs` soru dalı üç sonucu ayırıyor: `yes`, `no`, tanınmayan.
- `yes` ve `answer_yes` kenarı varsa kenar alınır; yoksa adım `done` olur
  (onay yolu).
- `no` ve `answer_no` kenarı yoksa adım **bloke** olur; `repairs/NN.md`
  hangi kenarın eksik olduğunu yazar. Eski davranış `done` yazıp `code`'a
  geçiyordu.
- Tanınmayan cevap ("maybe", boş `answer:`) ne onay ne ret sayılır; bloke
  edilir ve repair cevabı adlandırır.
- `yes, go ahead` gibi cümlelerin ilk kelimesi cevap sayılır; `answer_is_yes`
  yerine `answer_word` (`Yes` / `No` / `Other`) kullanılıyor.
- Varsayılan `.flow/workflow.toml` içindeki `approve-plan` hâlâ kenarsız;
  bu yüzden varsayılan akışta "No" bloke eder ve repair kenarı eklemeyi
  söyler. Kenarın hedefi/bütçesi bilinçli olarak eklenmedi (bkz. §3, madde
  17).
- Testler: `a_no_without_an_answer_no_edge_blocks_instead_of_approving`,
  `an_unrecognised_answer_does_not_stand_in_for_permission`.

### P0-2 — Commit protokolü tamamlandı, merge'in kirli kontrolü daraltıldı

- `start()` artık `flow: issue <slug>` commit'ini atıyor (AGENTS'in söz
  verdiği üç noktadan biri kodda yoktu).
- `Done` durumunda `flow: done <slug>` commit'i atılıyor; `summary.md` ve
  nihai `run.md` bir sonraki koşuya kalmıyor.
- `merge()` artık yalnızca `.scratch/` **dışındaki** kirli yolları reddediyor.
  Motorun kendi kaydı (`run.md`, sorular, repair'ler) merge'ü kilitlemiyor;
  başka birinin commit edilmemiş işi hâlâ reddediliyor.
- `DirtyMain` artık kirli yolları taşıyor; repair bu yolları adlandırıyor.
- `prepare::is_engine_path` tek tanım olarak merge ile paylaşılıyor.
- `AGENTS.md` dört commit noktasına ve `.scratch/` istisnasına göre
  güncellendi.
- Testler: mevcut merge testi artık `flow: issue demo`, `flow: done demo` ve
  temiz `git status` doğruluyor; `merge_probe` iki test:
  `a_dirty_engine_folder_does_not_refuse_the_merge`,
  `a_dirty_main_checkout_is_refused` (yol listesiyle).

### P0-3 — Süreçler arası koşu kilidi

- Yeni `crates/warp-flow-core/src/lock.rs`: `RunLock`, `std::fs::File::try_lock`
  ile `.git/warp-flow.lock` üzerinde (repo varsa) veya issue klasöründe
  (repo yoksa) kilit tutuyor.
- Kilit `Runner::build` içinde alınıyor ve `Runner` düşene kadar tutuluyor;
  süreç ölürse çekirdek bırakıyor.
- Kilit dosyası sahibini (pid, sürüm, zaman) yazıyor; ikinci motor
  "another warp-flow process is already driving this run" ile reddediliyor.
- Kısa bir yeniden deneme (20 × 10 ms) fork/exec penceresini atlatıyor: başka
  bir koşunun kapı komutu fork ettiği anda dosya tanımlayıcısı kısa süre
  miras kalabiliyor.
- Kilit `.git/` içinde olduğu için çalışma ağacını kirletmiyor.
- Test: `two_runners_cannot_drive_the_same_issue` (ikinci reddedilir, ilki
  düşünce üçüncü açılır).

### P1-1 — Kapı hatasının çıktısı kaybolmuyor

- `apply_report` bir kenarı almadan önce başarısız denemenin `Failure`'ını
  `repairs/NN.md` olarak yazıyor.
- Hedef adımın prompt'u, kenarı alan adımın en yeni repair'ini adlandırıyor;
  kapının `tail`'i artık `code` oturumuna ulaşıyor.
- Test: `a_gate_failure_writes_the_findings_the_target_reads` (ikinci ve
  üçüncü oturum sırasıyla `repairs/01.md`, `repairs/02.md` görüyor; dosyada
  `boom` var).

### P1-2 — Retry edge penceresini sıfırlıyor; merge idempotent

- `Runner::edge_floor`: bloke bir koşunun resume'unda (insan retry'ı) yalnızca
  o andan sonraki `sent-back` satırları edge turu sayılıyor.
- `MergeOutcome::AlreadyMerged`: branch `main`'in atasıysa ve `main`'de o
  branch ucunu ikinci ebeveyn olarak taşıyan bir merge commit'i varsa, görev
  yeniden merge edilmeden `merged` yazılıp çözülüyor. Bu,
  `git merge` ile satırın yazılması arasındaki çökme penceresini kapatıyor.
- Testler: `a_retry_resets_the_edge_window`,
  `a_merge_that_already_landed_is_reconciled_not_repeated`.

### P1-4 / P1-5 — Merge hataları bloke + repair; rebase yarım kalmıyor

- `merge()`'in döndürdüğü `Err` artık koşu hatası değil: `blocked` satırı,
  repair ve `Blocked` durumu.
- `merge::rebase_in_progress` / `merge::abort_rebase`: conflict adımı
  `Blocked`/`Waiting`/`Stopped` dönerse veya `Done` deyip rebase'i açık
  bırakırsa rebase iptal ediliyor; açık rebase ile ikinci merge denemesi
  başlamıyor.
- Testler: `a_merge_whose_checkout_is_on_another_branch_blocks_with_a_repair`;
  mevcut conflict testi hâlâ geçiyor.

### P1-6 — Kaybolan task dosyası grafiği "done" yapmıyor

- `apply_report` artık `Issue::open(...).ok()` ile hatayı yutmuyor: raporun
  adlandırdığı task okunamıyorsa veya yoksa adım bloke ediliyor ve repair
  yazılıyor.
- `mark_done` per-task bir adımı task olmadan işaretlemiyor
  (`debug_assert` + erken dönüş).
- Per-task bir adımın task'sız raporu da bloke ediliyor.
- Test: `a_per_task_report_whose_task_file_vanished_blocks`.

### P1-7 — Fresh başlatma mevcut kaydı ezmiyor

- `Host::begin`: `DriveMode::Fresh` ve `run.md` varsa "already has a run
  record; resume it instead" hatası. "Zaten sürülüyor" kontrolü önce
  çalışıyor, böylece sürülen bir koşu için mesaj hâlâ "already driving".
- Test: `a_fresh_start_over_an_existing_record_is_refused`.

### P1-8 — Worktree yalnızca gerçekten gönderilen iş için

- `drive` artık `max_parallel` kontrolünü `step_workdir`'den (yani
  `git worktree add`'den) **önce** yapıyor. Bir geçişte 8 hazır task varsa ve
  bütçe 2 ise yalnızca 2 worktree yaratılıyor.

### P2 — Küçük ama gerçek bulgular

- `RunnerConfig.factory = None` artık panic değil, `Err`.
- `graph::validate`: adım kimliklerinde boşluk, `|`, `@`, `,`, `:`, `/`
  reddediliyor (run.md tablosu, `running:` ve edge anahtarları bunları
  ayırıcı olarak kullanıyor).
- `produces` mutlak yol veya `..` içeremiyor; issue klasörü içinde kalıyor.
- `store::Run` "attempts" sayısı artık `sent-back`, `waiting` ve `skipped`
  satırlarını saymıyor.
- Terminal bir koşuda açık soru "Waiting on you" lane'ine ve cevaplanabilir
  karta dönüşmüyor; bloke kartı tarama notu (`run.note`) yerine motorun
  `**Run stopped:**` cümlesini gösteriyor.

## 2. Doğrulama

- `cargo test --locked -p warp-flow-core`: 209 birim testi + tüm probe'lar
  (bump 6, chain 4, door 6, merge 11, run 8, template 8) geçti.
- `cargo test --locked`: uygulama probe'ları geçti (75 birim + host, door,
  quit, store, UI probe'ları).
- `cargo clippy --locked -p warp-flow-core --all-targets -- -D warnings` ve
  `cargo clippy --locked --all-targets -- -D warnings`: temiz.
- `cargo fmt --all -- --check`: temiz.
- Kilit testi 10 ardışık koşuda kararlı; kısa fork/exec penceresi için
  yeniden deneme eklendi.

## 3. Kalan işler — karar gerektirenler

Aşağıdakiler bilerek yapılmadı; her biri bir ürün/sözleşme kararı istiyor.

| # | Konu | Neden karar gerekiyor | Öneri |
|---|---|---|---|
| 1 | Rebase sonrası kapı hatasının `code`'a dönüşü | Yeni bir `on_gate_failed` alanı şema + ADR değişikliği; alternatif "merge'i tekrar dene" politikası | `on_conflict` ile aynı şekilde per-task repair adımı |
| 2 | Bildirim kanalı | Masaüstü bildirimi mi, uygulama içi toast mı, ikisi mi; "rahatsız etme" ayarı | `notify-rust` + rail sayacı + başlık aktivitesi |
| 3 | Çoklu açık soru | Aynı anda birden çok soru gösterilsin mi, sırayla mı cevaplansın | Kartta "1/3" ve sıralı cevap |
| 4 | Ölü tetikler (`gate_passed`, gate `blocked`) | Uygulamak mı, `validate`'te reddetmek mi | Uygulamak: gate başarısında `GatePassed`, hatasında `Blocked` |
| 5 | Windows kapıları | `cmd /C` + `taskkill /T` yolu mu, "deneysel" etiketi mi | `run_once`'taki Windows yolunu kapıya uygula |
| 6 | `max_parallel` ayrımı | Oturum ve kapı bütçeleri ayrılsın mı; adım bazlı timeout | Ayrı sayaçlar + adım `timeout` alanı |
| 7 | Worktree tazeleme | Dispatch'te rebase mi, merge kuyruğunda toplu/spekülatif merge mi | Önce dispatch'te tazeleme, sonra toplu merge |
| 8 | `needs-info` task'ların join'i | Bu task'lar "çözülmüş" mü sayılmalı, yoksa koşu bloke mi olmalı | Açık karar; şimdilik summary/cleanup bekliyor |
| 9 | `edge_span` torunları | Hedefin atası olmayan hedefte aşağı akış yeniden koşsun mu | `dependents` kapanışı ile span genişletme |
| 10 | UI thread'inde `prepare` | `Host::start` hataları senkron mu kalmalı, faza mı taşınmalı | Worker'a taşı; API'yi "Starting" + phase error yap |
| 11 | Oturum yazıcısı / orphan oturumlar | `sessions/` scheduler'a nasıl taşınır; çökme sonrası oturum sahipliği | Door → scheduler kanalı + oturum kimliği |
| 12 | Uygulama tek örneği | İkinci pencereye izin var mı; endpoint çakışması nasıl çözülür | Tek örnek kilidi (mevcut kilit altyapısıyla) |
| 13 | `run.md` performansı | O(R²) yazma / O(R) edge taraması nasıl kırılır | Append-only JSONL yan dosya + kenar sayaçları |
| 14 | Tarama artımlılığı | 2 saniyelik tam tarama yerine `notify` + mtime önbelleği | Ayrı bir tarama iş parçacığı |
| 15 | AI'nın akışı şekillendirmesi | `plan.toml` / ad-hoc soru protokolü / adım politikaları büyük tasarım | Ayrı ADR ve şema revizyonu |
| 16 | Repos › Workflow kaydı hâlâ merge'ü bloke ediyor | `.flow/workflow.toml` izlenen bir dosya; kaydetmek `DirtyMain` üretiyor. Ya kayıtta commit atılmalı ya da merge `.flow`'u dışlamalı | Kayıt sırasında `flow: workflow <repo>` commit'i |
| 17 | Varsayılan `approve-plan` şablonunda `answer_no` kenarı yok | P0-1 düzeltmesinden sonra varsayılan akışta "No" bloke ediyor; plan reddinin nereye döneceği bir ürün kararı | `edges = [{ to = "analyze", when = "answer_no", max = 2 }]` |

## 4. Not

Bu çalışma **commit edilmedi**. Ağaçta yalnızca bu belge, inceleme belgesi ve
yukarıdaki kaynak değişiklikleri var. Bir sonraki adım, kalan tablodan
hangilerinin yapılacağına karar vermek; özellikle 1, 4 ve 6 numaralı maddeler
motorun sözleşmesini değiştiriyor.

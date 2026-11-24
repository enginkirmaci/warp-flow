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
3, 4, 12 ve 17 ikinci turda (§4), 1, 2, 8 ve 16 üçüncü turda (§6), kalanı
dördüncü turda (§7) karara bağlandı; 6 ve 7 bilinçli olarak ertelendi.

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

## 4. Uygulanan kararlar (2026-11-24, ikinci tur)

Kalan tablodan **3, 4, 12 ve 17** karara bağlandı ve uygulandı. Tablodaki
satırlar tarihsel kayıt olarak bırakıldı; güncel durum bu bölümdedir.

### 17 — Plan reddi analize geri dönüyor, detayıyla

- `.flow/workflow.toml` ve editörün yeni-depo menüsü (`new_graph`):
  `approve-plan` artık `edges = [{ to = "analyze", when = "answer_no",
  max = 2 }]` taşıyor. İki tur sonunda bloke olur.
- UI: "No" tek başına gönderilmiyor. Kart bir detay moduna geçiyor
  (`flow-question-details`), imleç cevap alanına gidiyor, düğme
  "Send back" oluyor ve gönderilen cevap `no: <detay>` oluyor.
- Motor: `answer_word` kelimenin sonundaki noktalama işaretlerini atıyor,
  böylece `no:` / `no,` / `no.` hep `no` okunuyor ve detay cevapta kalıyor.
- Prompt: bir edge alındığında hedefin prompt'u, soruyu soranın
  `consumed` durumundaki dosyasını ve cevabı adlandırıyor; yeni plan
  reddedilen planı körü körüne tekrarlamıyor.
- Testler: `a_rejection_with_details_sends_the_plan_back_and_names_them`,
  `the_repositorys_own_template_takes_a_rejection_back_to_the_plan`,
  `a_rejection_asks_for_the_detail_and_sends_it_with_the_no`.

### 4 — `gate_passed` ateşleniyor; gate `blocked` doğrulamada reddediliyor

- Sıfır çıkışlı bir gate artık `GatePassed` yükseltiyor; edge yoksa
  davranış değişmiyor.
- Bir gate `blocked` yükseltemez: oturumu yok, hatası çıkış kodudur.
  `Trigger::fits` ve `trigger_owner` buna göre daraltıldı; ADR-0023'ün
  tetik tablosu güncellendi.
- Testler: `a_gate_that_passes_may_send_the_work_on_its_gate_passed_edge`,
  `a_gate_may_loop_on_a_pass_but_not_on_blocked`.

### 3 — Birden çok açık soru tek kartta, sırayla

- Kart, açık soruların kaçıncısını gösterdiğini `1 of 2` çipiyle söylüyor
  (`flow-question-count`); cevaplanan soru kapanınca sıradaki kart geliyor.
  `store::Run::open_questions` zaten hepsini taşıyordu, UI yalnızca ilkini
  çiziyordu.
- Testler: `several_open_questions_name_the_first_of_them`.

### 12 — Tek uygulama örneği

- Yeni `src/instance.rs`: `<config>/instance.lock` üzerinde `File::try_lock`.
  İkinci başlatma "warp-flow is already running; leaving its window alone."
  yazıp çıkıyor; kilit süreç ömrü boyunca tutuluyor ve çekirdek bırakıyor.
- `--version` kilitten önce yanıtlanıyor, böylece aile About listesi açık bir
  warp-flow varken de satırını doldurabiliyor.
- Test: `a_second_instance_is_refused_and_a_dropped_one_frees_the_lock`.

## 5. Doğrulama ve durum

- İlk tur `1b2e382` olarak commit edildi (`fix(engine): a no is not a yes,
  one lock per run, and a closed record`), sürüm 0.1.67 → 0.1.68.
- İkinci turda: `cargo test --locked -p warp-flow-core` **212** birim testi +
  probe'lar; `cargo test --locked` **76** birim testi + probe'lar; her iki
  crate'te clippy `-D warnings` ve `cargo fmt --all -- --check` temiz.
- İkinci tur sonunda açık kalan kararlar: tablodaki 1, 2, 5–11 ve 13–16.

## 6. Üçüncü tur — kalan dört karar (2026-11-24)

### 1 — `on_gate_failed`: merge düşen kapının tamircisini adlandırıyor

- `Step.on_gate_failed` eklendi; `on_conflict` ile aynı şekle sahip olmalı:
  enabled, per_task harness, merge'den sonra tanımlı ve merge'i bekliyor.
  `Workflow::validate_named_repair_step` iki alanı tek yerden doğruluyor.
- Merge'in rebase sonrası kapısı düşünce motor repair'ı yazıyor, adlandırılan
  adımı merge'in turu içinde çalıştırıyor (bariyer duruyor) ve bir sonraki
  denemeyi yeni commit'lerle yapıyor. Adımın prompt'u merge'in en yeni
  repair'ını adlandırıyor.
- Şablon `.flow/workflow.toml`: merge `on_gate_failed = "fix-gates"` taşıyor
  ve `fix-gates` adlı per-task `code` adımı merge'den sonra tanımlı. Editörün
  yeni-depo menüsü adımı sunuyor ama merge'i onunla adlandırmıyor (menüde her
  adım kapalı; adlandırma için ikisi de açılmalı).
- Editör: merge formunda "On gate failure" alanı var.
- ADR-0024 yazıldı; AGENTS ve README merge kuralını güncelliyor.
- Testler: `a_failed_merge_gate_runs_the_named_fixer_and_lands_on_the_retry`,
  `a_merge_may_name_the_step_that_fixes_its_gates`,
  `a_gate_failure_step_must_wait_for_its_merge`,
  `the_repositorys_own_template_names_the_step_that_fixes_a_failed_gate`.

### 8 — Duran koşu bütün çözülmemiş task'ları durumlarıyla yazıyor

- `stall_reason` artık ilk task'ı değil hepsini adlandırıyor:
  `tasks 01 ... (needs-info), 02 ... (wontfix) are not resolved ...`.
- Tek task'lı durum da durum sözcüğünü taşıyor; `run.md`'nin
  `**Run stopped:**` satırı bloke kartında okunuyor.
- Testler: `a_stalled_run_names_every_unresolved_task_and_its_status`,
  `run_probe::a_task_never_starts_while_its_blocker_is_unresolved` güncellendi.

### 16 — Workflow kaydı: kod değişmedi, uyarı eklendi

- Karar: kayıt aynı anda commit etmiyor (UI git yazmıyor, merge commit'iyle
  yarışmıyor). Bunun yerine editör uyarıyor: kirli başlık satırı
  "commit before a merge" diyor ve başarılı kayıt mesajı `.flow/workflow.toml`
  commit edilene kadar merge'ün reddedileceğini söylüyor.
- Yeni bir kutu eklenmedi: başlığa satır eklemek formun ilk satırlarını
  kaydırıp tıklamaları kaydırıyordu; uyarı var olan durum satırlarına yazıldı.
- Davranış değişmedi: dosya commit edilene kadar `git status` kirli ve merge
  reddediyor.

### 2 — Bildirimler: iki kanal, olay bazlı ayarlar

- `store::Notifications`: `waiting`, `blocked`, `finished` olayları ve
  `desktop` kanalı. Yeni Settings › Notifications grubu bu dört anahtarı
  taşıyor; her olay için pencere kartı, istenirse masaüstü bildirimi.
- Uygulama içi: sağ altta en fazla üç kart (`flow-notices`, 8 saniye sonra
  kendiliğinden kapanır), çalışan koşuda başlık "N waiting on you", rail
  pip'i zaten `need_you_count` gösteriyor.
- Masaüstü: `notify-rust` (gpui'nin Linux tarafında zaten ağaçta), ayrı kısa
  ömürlü thread'de; DBus yoksa yalnızca debug satırı. Varsayılan kapalı:
  pencere kartı yeterli, masaüstü istenirse açılıyor.
- İlk tarama yalnızca durum haritasını dolduruyor; açılışta eski koşular için
  kart üretilmiyor.
- Testler: `notify::tests::a_question_outranks_the_state_but_not_a_finished_run`,
  `settings_probe` dört satırı ve kalıcılığı doğruluyor.

### Üçüncü tur doğrulaması

- `cargo test --locked -p warp-flow-core`: **216** birim testi + probe'lar.
- `cargo test --locked`: **77** birim testi + probe'lar.
- Her iki crate'te clippy `-D warnings` ve `cargo fmt --all -- --check` temiz.
- Açık kalan kararlar: 5 (Windows kapıları), 6 (`max_parallel` ayrımı),
  7 (worktree tazeleme), 9 (`edge_span` torunları), 10 (UI thread'inde
  `prepare`), 11 (oturum yazıcısı), 13 (`run.md` performansı),
  14 (artımlı tarama), 15 (AI'nın akışı şekillendirmesi).

## 7. Dördüncü tur — kalan dokuz karar (2026-11-24)

### 5 — Windows kapıları

- `Gate::run` artık platform ayrımı yapmıyor: Unix'te `sh -c`, Windows'ta
  `cmd /C` + `CREATE_NO_WINDOW`, zaman aşımında `kill_tree` Windows'ta
  `taskkill /T` kullanıyor. Motorun kapısı ile ekranın tek seferlik
  `run_once` yolu aynı `execute` fonksiyonundan geçiyor.
- Burada Windows çalıştırılamadı; `cargo check -p warp-flow-core --target
  x86_64-pc-windows-msvc` temiz (cfg(windows) kodu derleniyor).

### 6 — `max_parallel` ayrımı: bırakıldı

- Karar: tek bütçe ve door'un küresel sessizlik/tavan değerleri kalıyor.
  Küçük grafiklerde fark etmiyor; ayrım ancak gerçek bir darboğaz görülürse
  yapılacak.
- 7 — worktree tazeleme de bırakıldı: kuyruk zaten rebase ediyor ve yeni
  `on_gate_failed` düşen kapıyı onarıyor (ADR-0024).

### 9 — `edge_span` torunları

- Hedef sender'ın atası değilse kapsam artık yalnız hedef değil, hedef **ve
  onu okuyan her adım** (`Workflow::downstream_of`, `resolve` üzerinden
  dolayısıyla disabled adımlar şeffaf). Eski davranış hedefi yeniden
  koşturup okuyucuları "done" bırakıyordu.
- Test: `a_sibling_edge_reruns_the_target_and_everything_that_read_it`.

### 10 + 11 — Host sahipliği

- `prepare`, `pin_workflow` ve `require_clean` artık worker'da: `start` /
  `resume` yalnız ucuz ve senkron kararları veriyor (kanonik klasör,
  done/resume/fresh kayıt kontrolü, depo başına tek koşu — depo `.git`
  dosyasından bulunuyor). Worker'ın reddi koşunun `Failed` fazı ve hatası
  oluyor; pencere bunu zaten çiziyor.
- `RunState`'in `repo` ve `title` alanları mutex: slot oluştuktan sonra
  worker dolduruyor, snapshot yanlış tahmin okumuyor.
- Oturum dosyaları artık door tarafından değil **scheduler** tarafından
  yazılıyor: `StepRequest` paylaşılan bir `SessionRecord` yuvası taşıyor,
  worker raporu `session` alanıyla dönüyor, `session::write` scheduler
  thread'inde çalışıyor (merge'in kendi turunda çalıştırdığı adımlar dahil).
- Testler: `host_probe` retleri artık faz/hata üzerinden bekliyor,
  `actions_probe` UI hatasını failed fazında okuyor, `door_probe` kaydın
  istek üzerinde bırakıldığını doğruluyor.

### 13 + 14 — Performans: artımlı tarama

- `store::Scanner`: issue klasörleri dosya adı/boyut/mtime parmak iziyle
  önbellekleniyor; değişmeyen klasör son `Run`'ı klonluyor. Kapı sayısı
  workflow dosyasının metadata'sıyla, kirli işareti 10 saniyelik TTL ile
  önbellekte. Pencere tarama döngüsü tek bir `Scanner` sahibi; `store::scan`
  probe'lar için tek seferlik sarmalayıcı olarak kaldı.
- `run.md`'nin satır başına yeniden yazılması bu turda ele alınmadı: ölçüm
  olmadan yan dosya formatı değiştirmek erken bir karmaşıklık olurdu.
- Test: `a_scanner_reuses_an_unchanged_folder_and_reads_a_changed_one`.

### 15 — Oturum içinde soru: `ask` protokolü

- Her harness adımının prompt'u protokolü taşıyor: karar veremiyorsa
  `questions/ask-<step>[-task-NN].md` dosyasını `kind: question`,
  `asked_by: <step>`, `state: open` (per-task'ta `task: NN`) ile yazar ve
  durur; `produces` dosyasını yazmaz.
- Motor oturum döndükten sonra açık soruyu görürse `waiting` satırı yazar,
  adımı "done" yapmaz ve koşu `Waiting` olur. Açık soru varken adım yeniden
  gönderilmez.
- Cevap gelince (`state: answered`) motor soruyu `consumed` yapar, cevabı
  prompt'a koyar ve adımı yeniden çalıştırır. `waiting` satırı, eski bir
  soru dosyasının hiç sormamış bir koşuyu kilitlemesini engelliyor.
- Merge'in adlandırdığı adım soru sorarsa merge turu beklemez: bloke eder
  ve repair soruyu adlandırır; sonraki merge denemesi cevabı okur.
- ADR-0025 yazıldı; README ve AGENTS protokolü anıyor.
- Test: `a_harness_session_may_stop_and_ask_and_reads_the_answer_when_it_resumes`.

### Dördüncü tur doğrulaması

- `cargo test --locked -p warp-flow-core`: **218** birim testi + probe'lar.
- `cargo test --locked`: **78** birim testi + probe'lar.
- Her iki crate'te clippy `-D warnings` ve `cargo fmt --all -- --check` temiz;
  `warp-flow-core` Windows hedefiyle `cargo check` temiz.
- Açık karar kalmadı; 6 ve 7 bilinçli olarak ertelendi, 13'ün `run.md`
  kısmı ölçüm bekliyor.
